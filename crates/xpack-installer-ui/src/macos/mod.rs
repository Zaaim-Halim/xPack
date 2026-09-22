//! The macOS wizard: one `NSWindow` drawn with `AppKit`'s own controls.
//!
//! The window is rebuilt from [`Wizard`] after everything the person does, so
//! it can never show a state the model is not in. Only the progress bar and
//! its two lines are updated in place while installing, because they change
//! many times a second.
//!
//! # The event loop is run here, one event at a time
//!
//! Rather than `NSApplication::run`, which returns only through a stop
//! request that needs an event to arrive before it takes effect. Taking events
//! one at a time with a short timeout lets the loop collect the installation's
//! progress between them and end the moment the wizard does, without another
//! thread ever touching `AppKit`.
//!
//! [`controller`] holds the target that buttons send their actions to, and
//! every call the bindings mark unsafe. [`pages`] draws each page.

mod controller;
mod pages;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::NSAppearanceCustomization;
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSEventMask, NSImage, NSModalResponseOK, NSOpenPanel, NSProgressIndicator, NSTextField,
    NSWindow,
};
use objc2_foundation::{NSData, NSDate, NSString};

use crate::engine::Engine;
use crate::model::{Key, Texts, Wizard};
use crate::session::{Host, Session, Update};
use crate::window::Shown;
use controller::{Controller, dark_appearance, default_run_loop_mode, new_window};

/// The window's content size, in points.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 440.0;
/// Outer padding and the gap between the sidebar and the main column.
const MARGIN: f64 = 20.0;
const SIDEBAR: f64 = 168.0;
/// The main column's left edge and width.
const MAIN_X: f64 = MARGIN + SIDEBAR + MARGIN;
const MAIN_WIDTH: f64 = WIDTH - MAIN_X - MARGIN;
/// The button row.
const BUTTON_HEIGHT: f64 = 28.0;
const BUTTON_WIDTH: f64 = 110.0;
/// The content box, between the title and the row under it.
const BOX_BOTTOM: f64 = MARGIN + BUTTON_HEIGHT + 44.0;
const BOX_TOP: f64 = HEIGHT - MARGIN - 34.0;
/// Padding inside the content box.
const INSET: f64 = 16.0;
/// How long the loop waits for an event before looking at the installation.
const TICK_SECONDS: f64 = 0.05;

// --- the run -----------------------------------------------------------------

thread_local! {
    /// The running wizard. Only ever touched on the main thread.
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` against the wizard, unless it is already being run: an action that
/// arrives while a modal panel of ours is open is dropped rather than
/// re-entering.
fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(app) = slot.as_mut()
        {
            f(app);
        }
    });
}

/// Shows the wizard until it ends.
pub(super) fn run(wizard: Wizard, engine: Arc<dyn Engine>, icon: Option<&[u8]>) -> Shown {
    let Some(mtm) = MainThreadMarker::new() else {
        return Shown::NotShown;
    };
    let application = NSApplication::sharedApplication(mtm);
    bring_forward(&application);

    let app = App::new(mtm, wizard, engine, icon);
    app.window.setDelegate(Some(ProtocolObject::from_ref(&*app.controller)));
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|app| {
        app.render();
        app.window.center();
        app.window.makeKeyAndOrderFront(None);
    });

    application.finishLaunching();
    let conclusion = loop {
        let deadline = NSDate::dateWithTimeIntervalSinceNow(TICK_SECONDS);
        if let Some(event) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&deadline),
            default_run_loop_mode(),
            true,
        ) {
            application.sendEvent(&event);
        }
        application.updateWindows();

        let mut ended = None;
        with_app(|app| {
            app.tick();
            ended = app.session.ended();
        });
        if let Some(conclusion) = ended {
            break conclusion;
        }
    };

    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().take() {
            app.window.orderOut(None);
        }
    });
    Shown::Ended(conclusion)
}

/// Draws the wizard as it is into a TIFF image, without showing a window.
///
/// So its pages can be looked at, in either appearance, on a machine where
/// nothing may be put on the screen or captured from it. Drawn by exactly the
/// code that draws the real window.
pub(super) fn snapshot(
    wizard: Wizard,
    engine: Arc<dyn Engine>,
    icon: Option<&[u8]>,
    dark: bool,
) -> Option<Vec<u8>> {
    let mtm = MainThreadMarker::new()?;
    let _ = NSApplication::sharedApplication(mtm);
    let app = App::new(mtm, wizard, engine, icon);
    if dark {
        app.window.setAppearance(dark_appearance().as_deref());
    }
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    let mut image = None;
    with_app(|app| {
        app.render();
        let Some(content) = app.window.contentView() else {
            return;
        };
        content.layoutSubtreeIfNeeded();
        let bounds = content.bounds();
        if let Some(rep) = content.bitmapImageRepForCachingDisplayInRect(bounds) {
            content.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
            image = rep.TIFFRepresentation().map(|data| data.to_vec());
        }
    });
    APP.with(|cell| cell.borrow_mut().take());
    image
}

/// Shows a message in place of the wizard.
pub(super) fn alert(title: &str, body: &str) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    bring_forward(&NSApplication::sharedApplication(mtm));
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    alert.runModal();
    true
}

/// Makes this process a regular application in front of the others.
///
/// It is started from Finder as the executable inside a bundle, but a window
/// from a process macOS still treats as a background one opens behind
/// whatever the person is looking at.
fn bring_forward(application: &NSApplication) {
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    application.activateIgnoringOtherApps(true);
}

/// An image from the bytes of an icon file in any format `AppKit` reads.
fn image_from(bytes: &[u8]) -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(bytes);
    NSImage::initWithData(NSImage::alloc(), &data)
}

/// The progress widgets, kept so they can be updated without a rebuild.
struct Live {
    status: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
    counts: Retained<NSTextField>,
    determinate: bool,
}

/// Everything the running wizard owns.
struct App {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    controller: Retained<Controller>,
    session: Session,
    dialogs: Dialogs,
    icon: Option<Retained<NSImage>>,
    /// The location field on the current page, to read before moving on.
    root_field: Option<Retained<NSTextField>>,
    live: Option<Live>,
}

impl App {
    fn new(
        mtm: MainThreadMarker,
        wizard: Wizard,
        engine: Arc<dyn Engine>,
        icon: Option<&[u8]>,
    ) -> Self {
        let controller = Controller::new(mtm);
        let window = new_window(mtm, &wizard.texts().line(Key::WindowTitle));
        Self {
            mtm,
            window,
            controller,
            // The run loop looks at the installation every tick, so there is
            // nothing to wake.
            session: Session::new(wizard, engine, Arc::new(|| {})),
            dialogs: Dialogs { mtm },
            icon: icon.and_then(image_from),
            root_field: None,
            live: None,
        }
    }

    /// Redraws what changed.
    fn apply(&mut self, update: Update) {
        match update {
            Update::Nothing => {}
            Update::Progress => self.update_live(),
            Update::Page => self.render(),
        }
    }

    // --- what the person did ---------------------------------------------------

    fn back(&mut self) {
        self.commit_root();
        let update = self.session.back();
        self.apply(update);
    }

    fn primary(&mut self) {
        // A path typed and not yet confirmed counts: the button says what the
        // field says.
        self.commit_root();
        let update = self.session.primary(&self.dialogs);
        self.apply(update);
    }

    fn close_requested(&mut self) {
        self.session.close_requested(&self.dialogs);
    }

    fn browse(&mut self) {
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setCanCreateDirectories(true);
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let chosen = panel.URLs().firstObject().and_then(|url| url.path());
        if let Some(path) = chosen {
            let update = self.session.set_root(PathBuf::from(path.to_string()));
            self.apply(update);
        }
    }

    /// Reads the location field into the model.
    fn commit_root(&mut self) {
        let Some(field) = &self.root_field else {
            return;
        };
        let typed = PathBuf::from(field.stringValue().to_string());
        let update = self.session.set_root(typed);
        self.apply(update);
    }

    fn retry(&mut self) {
        let update = self.session.retry();
        self.apply(update);
    }

    fn launch_existing(&mut self) {
        self.session.launch_existing(&self.dialogs);
    }

    fn toggle(&mut self, change: impl FnOnce(&mut Wizard)) {
        let update = self.session.change(change);
        self.apply(update);
    }

    // --- between events ---------------------------------------------------------

    fn tick(&mut self) {
        let update = self.session.tick();
        self.apply(update);
    }
}

/// The questions only `AppKit` can ask.
struct Dialogs {
    mtm: MainThreadMarker,
}

impl Host for Dialogs {
    fn confirm_cancel(&self, texts: &Texts) -> bool {
        let alert = NSAlert::new(self.mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.setMessageText(&NSString::from_str(&texts.line(Key::CancelTitle)));
        alert.setInformativeText(&NSString::from_str(&texts.line(Key::CancelBody)));
        // First is the default, and the default keeps the wizard open.
        alert.addButtonWithTitle(&NSString::from_str(&texts.line(Key::CancelKeep)));
        alert.addButtonWithTitle(&NSString::from_str(&texts.line(Key::CancelConfirm)));
        alert.runModal() == NSAlertSecondButtonReturn
    }

    fn show_error(&self, title: &str, body: &str) {
        alert(title, body);
    }
}
