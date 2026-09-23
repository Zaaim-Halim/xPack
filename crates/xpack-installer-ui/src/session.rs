//! A wizard being shown: what each thing the person does leads to.
//!
//! The same on every platform, so it lives here rather than in each
//! front-end: what the primary button does on each page, when closing asks
//! first, starting and collecting the installation, launching afterwards and
//! reporting a launch that failed. A front-end draws, and answers the two
//! questions only it can ask, through [`Host`].

use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::Engine;
use crate::installation::{Installation, Wake};
use crate::model::{CloseRequest, Conclusion, Key, Step, Texts, Wizard};

/// What only the platform can do: ask, and tell.
pub(crate) trait Host {
    /// Asks whether to leave before anything was installed. `true` to leave.
    fn confirm_cancel(&self, texts: &Texts) -> bool;
    /// Shows an error that does not end the run.
    fn show_error(&self, title: &str, body: &str);
}

/// What changed, so a front-end redraws no more than it must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Update {
    /// Nothing visible changed.
    Nothing,
    /// Only the installation's progress changed.
    Progress,
    /// The page, or something on it, changed.
    Page,
}

/// A wizard being shown, with the installer behind it.
pub(crate) struct Session {
    wizard: Wizard,
    engine: Arc<dyn Engine>,
    installation: Option<Installation>,
    ended: Option<Conclusion>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Session {
    /// A session on the wizard's first page.
    ///
    /// `wake` is called from the installation's worker thread whenever there
    /// is something to collect; it must only nudge the window's thread, which
    /// then calls [`Self::tick`].
    pub(crate) fn new(
        wizard: Wizard,
        engine: Arc<dyn Engine>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let mut session = Self { wizard, engine, installation: None, ended: None, wake };
        session.inspect_if_needed();
        session
    }

    /// The wizard, to draw from.
    pub(crate) fn wizard(&self) -> &Wizard {
        &self.wizard
    }

    /// How the run ended, once it has.
    pub(crate) fn ended(&self) -> Option<Conclusion> {
        self.ended
    }

    /// The primary button: Next, Install or Close.
    pub(crate) fn primary(&mut self, host: &dyn Host) -> Update {
        if self.wizard.finish_view().is_some() {
            self.finish(host);
            return Update::Nothing;
        }
        self.inspect_if_needed();
        match self.wizard.advance() {
            Step::Moved(_) => {}
            Step::Install(choices) => {
                let wake = Arc::clone(&self.wake);
                let wake: Wake = Box::new(move || wake());
                self.installation =
                    Some(Installation::start(Arc::clone(&self.engine), choices, wake));
            }
            Step::Refused => return Update::Nothing,
        }
        Update::Page
    }

    /// Back.
    pub(crate) fn back(&mut self) -> Update {
        if self.wizard.back().is_some() { Update::Page } else { Update::Nothing }
    }

    /// A root typed into the location field or chosen in a folder picker.
    pub(crate) fn set_root(&mut self, root: PathBuf) -> Update {
        if root == self.wizard.root() || !self.wizard.set_root(root) {
            return Update::Nothing;
        }
        self.inspect_if_needed();
        Update::Page
    }

    /// Retry, beside a busy installation.
    pub(crate) fn retry(&mut self) -> Update {
        self.wizard.retry();
        self.inspect_if_needed();
        Update::Page
    }

    /// A checkbox, or anything else that only changes the model.
    pub(crate) fn change(&mut self, change: impl FnOnce(&mut Wizard)) -> Update {
        change(&mut self.wizard);
        Update::Page
    }

    /// Launch, beside "already installed". Ends the run.
    pub(crate) fn launch_existing(&mut self, host: &dyn Host) {
        if let Some(root) = self.wizard.launch_existing() {
            self.launch(&root, host);
            self.ended = self.wizard.conclusion();
        }
    }

    /// Cancel, or the window's close button.
    pub(crate) fn close_requested(&mut self, host: &dyn Host) {
        match self.wizard.request_close() {
            CloseRequest::Refuse => {}
            CloseRequest::Close => self.finish(host),
            CloseRequest::Confirm => {
                if host.confirm_cancel(self.wizard.texts()) {
                    self.wizard.confirm_cancel();
                    self.ended = self.wizard.conclusion();
                }
            }
        }
    }

    /// Collects what the installation reported, and answers an inspection the
    /// model is waiting for.
    pub(crate) fn tick(&mut self) -> Update {
        let mut update = if self.inspect_if_needed() { Update::Page } else { Update::Nothing };
        if let Some(installation) = &mut self.installation {
            if installation.deliver(&mut self.wizard) {
                self.installation = None;
                update = Update::Page;
            } else if update == Update::Nothing {
                update = Update::Progress;
            }
        }
        update
    }

    /// Leaves the last page, launching the application if the person asked.
    fn finish(&mut self, host: &dyn Host) {
        if let Some(root) = self.wizard.close() {
            self.launch(&root, host);
        }
        self.ended = self.wizard.conclusion();
    }

    /// Starts the installed application. A failure is shown, and changes
    /// nothing about how the run ended: the installation itself succeeded.
    fn launch(&self, root: &std::path::Path, host: &dyn Host) {
        if let Err(error) = self.engine.launch(root) {
            host.show_error(&self.wizard.texts().line(Key::LaunchFailedTitle), &error.to_string());
        }
    }

    /// Asks the engine about the root, when the model is waiting to know.
    fn inspect_if_needed(&mut self) -> bool {
        let Some(root) = self.wizard.pending_inspection().map(std::path::Path::to_path_buf) else {
            return false;
        };
        let inspection = self.engine.inspect(&root);
        self.wizard.inspected(&root, inspection);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use xpack_core::{Error, ProgressReporter, Version};
    use xpack_install::Existing;

    use super::*;
    use crate::engine::{Choices, Inspection, Installed};
    use crate::model::{Facts, Flavour, Page, UiPlan, WizardSpec};

    /// An engine whose install succeeds and whose launch fails, recording both.
    struct Engine2 {
        existing: Existing,
        launched: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl Engine for Engine2 {
        fn inspect(&self, root: &Path) -> Inspection {
            Inspection { target: root.join("app"), verdict: Ok(self.existing.clone()) }
        }

        fn install(&self, _: &Choices, _: &dyn ProgressReporter) -> Result<Installed, Error> {
            Ok(Installed {
                directory: PathBuf::from("/r/app"),
                version: Version::parse("2.0.0").unwrap(),
                shortcut_added: false,
            })
        }

        fn launch(&self, root: &Path) -> Result<(), Error> {
            self.launched.lock().unwrap().push(root.to_path_buf());
            Err(Error::invalid("launch", "it would not start"))
        }
    }

    /// Answers the confirmation as told and records every error shown.
    struct Recorder {
        leave: bool,
        errors: RefCell<Vec<String>>,
    }

    impl Host for Recorder {
        fn confirm_cancel(&self, _: &Texts) -> bool {
            self.leave
        }

        fn show_error(&self, title: &str, _: &str) {
            self.errors.borrow_mut().push(title.to_string());
        }
    }

    fn session(existing: Existing, launch_on_finish: bool) -> (Session, Arc<Engine2>) {
        let engine = Arc::new(Engine2 { existing, launched: std::sync::Mutex::default() });
        let wizard = Wizard::new(WizardSpec {
            flavour: Flavour::Windows,
            facts: Facts {
                name: "App".into(),
                version: Version::parse("2.0.0").unwrap(),
                publisher: None,
                description: None,
            },
            plan: UiPlan { launch_on_finish, ..UiPlan::default() },
            licence: None,
            shortcut_requested: false,
            root: PathBuf::from("/r"),
            root_fixed: false,
            log: None,
        });
        let as_engine: Arc<dyn Engine> = Arc::clone(&engine) as Arc<dyn Engine>;
        (Session::new(wizard, as_engine, Arc::new(|| {})), engine)
    }

    fn host(leave: bool) -> Recorder {
        Recorder { leave, errors: RefCell::default() }
    }

    /// Presses the primary button until the installation has finished.
    fn install(session: &mut Session, host: &Recorder) {
        while session.wizard().page() != Page::Installing {
            assert_eq!(
                session.primary(host),
                Update::Page,
                "stuck on {:?}",
                session.wizard().page()
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.wizard().page() == Page::Installing {
            assert!(Instant::now() < deadline, "the installation never ended");
            session.tick();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_first_root_is_inspected_before_anything_is_drawn() {
        let (session, _) = session(Existing::Nothing, false);
        assert!(session.wizard().pending_inspection().is_none());
    }

    #[test]
    fn a_failed_launch_is_shown_and_the_run_still_counts_as_installed() {
        let (mut session, engine) = session(Existing::Nothing, true);
        let host = host(false);
        install(&mut session, &host);

        assert_eq!(session.primary(&host), Update::Nothing);
        assert_eq!(engine.launched.lock().unwrap().as_slice(), [PathBuf::from("/r")]);
        assert_eq!(host.errors.borrow().as_slice(), ["App could not be started"]);
        assert_eq!(session.ended(), Some(Conclusion::Installed));
    }

    #[test]
    fn closing_before_installing_asks_and_staying_changes_nothing() {
        let (mut session, _) = session(Existing::Nothing, false);
        session.close_requested(&host(false));
        assert_eq!(session.ended(), None);
        session.close_requested(&host(true));
        assert_eq!(session.ended(), Some(Conclusion::Cancelled));
    }

    #[test]
    fn nothing_is_launched_unless_the_publisher_offered_it() {
        let (mut session, engine) = session(Existing::Nothing, false);
        let host = host(false);
        install(&mut session, &host);
        session.close_requested(&host);
        assert!(engine.launched.lock().unwrap().is_empty());
        assert_eq!(session.ended(), Some(Conclusion::Installed));
    }

    #[test]
    fn opening_what_is_already_installed_ends_the_run() {
        let (mut session, engine) = session(Existing::Installed, false);
        let host = host(false);
        while session.wizard().page() != Page::Location {
            session.primary(&host);
        }
        session.launch_existing(&host);
        assert_eq!(engine.launched.lock().unwrap().len(), 1);
        assert_eq!(session.ended(), Some(Conclusion::AlreadyInstalled));
    }

    #[test]
    fn back_returns_to_the_previous_page_and_not_past_the_first() {
        let (mut session, _) = session(Existing::Nothing, false);
        assert_eq!(session.back(), Update::Nothing, "the first page has nothing before it");
        session.primary(&host(false));
        let moved_to = session.wizard().page();
        assert_eq!(session.back(), Update::Page);
        assert!(session.wizard().page() < moved_to);
    }

    #[test]
    fn a_change_to_the_model_redraws_the_page() {
        let (mut session, _) = session(Existing::Nothing, true);
        assert_eq!(session.change(|wizard| wizard.set_launch(false)), Update::Page);
        assert!(!session.wizard().launch());
    }

    #[test]
    fn retry_asks_about_the_root_again_at_once() {
        let (mut session, _) = session(Existing::Busy, false);
        while session.wizard().page() != Page::Location {
            session.primary(&host(false));
        }
        assert_eq!(session.retry(), Update::Page);
        assert!(session.wizard().pending_inspection().is_none(), "answered, not left pending");
    }

    #[test]
    fn an_unchanged_root_is_not_inspected_again() {
        let (mut session, _) = session(Existing::Nothing, false);
        assert_eq!(session.set_root(PathBuf::from("/r")), Update::Nothing);
        assert_eq!(session.set_root(PathBuf::from("/elsewhere")), Update::Page);
        assert!(session.wizard().pending_inspection().is_none(), "inspected at once");
    }
}
