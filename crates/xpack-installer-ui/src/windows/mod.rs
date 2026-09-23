//! The Windows wizard: one window of stock Win32 controls.
//!
//! Everything is a system control, so the window takes its font, colours,
//! scaling and screen-reader behaviour from Windows rather than reimplementing
//! any of it. There is no custom colour anywhere, which keeps high contrast
//! working.
//!
//! Clicks and the installation's progress arrive as events on this thread;
//! the worker wakes it with a [`nwg::Notice`], which carries nothing, so the
//! progress itself is collected from the shared [`Session`].
//!
//! [`controls`] creates the window and lays each page out.

mod controls;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use native_windows_gui as nwg;

use crate::engine::Engine;
use crate::model::{Key, Texts, Visibility, Wizard};
use crate::session::{Host, Session, Update};
use crate::window::Shown;
use controls::{Action, Controls};

/// Shows the wizard until it ends.
pub(super) fn run(wizard: Wizard, engine: Arc<dyn Engine>, icon: Option<&[u8]>) -> Shown {
    if !start() {
        return Shown::NotShown;
    }
    let Ok(controls) = Controls::build(&wizard.texts().line(Key::WindowTitle), icon) else {
        return Shown::NotShown;
    };

    let sender = controls.notice.sender();
    let wake = Arc::new(move || sender.notice());
    let app = Rc::new(RefCell::new(App {
        dialogs: Dialogs { window: controls.window.handle },
        session: Session::new(wizard, engine, wake),
        controls,
    }));

    let weak = Rc::downgrade(&app);
    let handler = {
        let window = app.borrow().controls.window.handle;
        nwg::full_bind_event_handler(&window, move |event, data, handle| {
            // Dropped rather than re-entered: a modal message box runs its own
            // loop, and an event arriving through it must not borrow twice.
            if let Some(app) = weak.upgrade()
                && let Ok(mut app) = app.try_borrow_mut()
            {
                app.handle(event, &data, handle);
                if app.session.ended().is_some() {
                    nwg::stop_thread_dispatch();
                }
            }
        })
    };

    {
        let app = app.borrow();
        app.controls.render(app.session.wizard());
        app.controls.window.set_visible(true);
    }
    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);

    let app = app.borrow();
    app.controls.window.set_visible(false);
    app.session.ended().map_or(Shown::NotShown, Shown::Ended)
}

/// Shows a message in place of the wizard.
pub(super) fn alert(title: &str, body: &str) -> bool {
    if !start() {
        return false;
    }
    nwg::message(&nwg::MessageParams {
        title,
        content: body,
        buttons: nwg::MessageButtons::Ok,
        icons: nwg::MessageIcons::Warning,
    });
    true
}

/// Prepares the toolkit. `false` when no window can be shown.
///
/// DPI awareness is not set here. It is declared in the manifest written into
/// the installer when it is branded, which is where Windows wants it and needs
/// no unsafe call; the toolkit then scales every position it is given.
fn start() -> bool {
    nwg::init().is_ok()
}

/// Everything the running wizard owns.
struct App {
    controls: Controls,
    session: Session,
    dialogs: Dialogs,
}

impl App {
    fn handle(&mut self, event: nwg::Event, data: &nwg::EventData, handle: nwg::ControlHandle) {
        match event {
            nwg::Event::OnButtonClick => {
                if let Some(action) = self.controls.action_for(handle) {
                    self.act(action);
                }
            }
            nwg::Event::OnWindowClose if handle == self.controls.window.handle => {
                // Never closed from here: the dispatch loop ends when the
                // wizard does, and the window is hidden then.
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
                self.session.close_requested(&self.dialogs);
            }
            // The dialog manager turns Return and Escape into these, whatever
            // has focus. Return does what the primary button would, and only
            // when it could be pressed; Escape is Cancel, with its question.
            nwg::Event::OnKeyEnter => {
                if self.session.wizard().buttons().primary == Visibility::Enabled {
                    self.act(Action::Primary);
                }
            }
            nwg::Event::OnKeyEsc => self.session.close_requested(&self.dialogs),
            nwg::Event::OnNotice => {
                let update = self.session.tick();
                self.apply(update);
            }
            _ => {}
        }
    }

    fn act(&mut self, action: Action) {
        let update = match action {
            Action::Back => {
                self.commit_root();
                self.session.back()
            }
            Action::Primary => {
                // A path typed and not yet confirmed counts: the button says
                // what the field says.
                self.commit_root();
                self.session.primary(&self.dialogs)
            }
            Action::Cancel => {
                self.session.close_requested(&self.dialogs);
                Update::Nothing
            }
            Action::Browse => self.browse(),
            Action::Retry => self.session.retry(),
            Action::LaunchExisting => {
                self.session.launch_existing(&self.dialogs);
                Update::Nothing
            }
            Action::Accept => {
                let on = is_checked(&self.controls.accept);
                self.session.change(|wizard| wizard.set_accepted(on))
            }
            Action::Shortcut => {
                let on = is_checked(&self.controls.shortcut);
                self.session.change(|wizard| wizard.set_shortcut(on))
            }
            Action::Launch => {
                let on = is_checked(&self.controls.launch);
                self.session.change(|wizard| wizard.set_launch(on))
            }
        };
        self.apply(update);
    }

    /// Redraws what changed.
    fn apply(&self, update: Update) {
        match update {
            Update::Nothing => {}
            Update::Progress => self.controls.update_progress(self.session.wizard()),
            Update::Page => self.controls.render(self.session.wizard()),
        }
    }

    /// Reads the location field into the model, when it is on screen.
    fn commit_root(&mut self) {
        if self.controls.root.visible() {
            let typed = PathBuf::from(self.controls.root.text());
            let update = self.session.set_root(typed);
            self.apply(update);
        }
    }

    fn browse(&mut self) -> Update {
        let mut dialog = nwg::FileDialog::default();
        let built = nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::OpenDirectory)
            .build(&mut dialog);
        if built.is_err() || !dialog.run(Some(&self.controls.window)) {
            return Update::Nothing;
        }
        match dialog.get_selected_item() {
            Ok(chosen) => self.session.set_root(PathBuf::from(chosen)),
            Err(_) => Update::Nothing,
        }
    }
}

fn is_checked(checkbox: &nwg::CheckBox) -> bool {
    checkbox.check_state() == nwg::CheckBoxState::Checked
}

/// The questions only Windows can ask.
struct Dialogs {
    window: nwg::ControlHandle,
}

impl Host for Dialogs {
    fn confirm_cancel(&self, texts: &Texts) -> bool {
        // A message box cannot relabel its buttons, so the question is worded
        // to be answered Yes or No.
        let choice = nwg::modal_message(
            self.window,
            &nwg::MessageParams {
                title: &texts.line(Key::CancelTitle),
                content: &texts.line(Key::CancelBody),
                buttons: nwg::MessageButtons::YesNo,
                icons: nwg::MessageIcons::Question,
            },
        );
        choice == nwg::MessageChoice::Yes
    }

    fn show_error(&self, title: &str, body: &str) {
        nwg::modal_message(
            self.window,
            &nwg::MessageParams {
                title,
                content: body,
                buttons: nwg::MessageButtons::Ok,
                icons: nwg::MessageIcons::Error,
            },
        );
    }
}
