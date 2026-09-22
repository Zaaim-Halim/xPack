//! The object buttons send their actions to, and every unsafe call.
//!
//! `AppKit` tells a button what to do by giving it a target object and a
//! selector, and the bindings mark every call that sets them up as unsafe:
//! defining the target class, creating it, and creating buttons pointed at
//! it. So do creating a window, starting a progress bar's animation and
//! reading the constants `AppKit` and Foundation export. Each is one small function here
//! with its reasoning beside it; nothing else in the front-end is unsafe.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValueOff, NSControlStateValueOn,
    NSFontWeightSemibold, NSProgressIndicator, NSTextField, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};

use super::{App, HEIGHT, WIDTH, with_app};

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Controller` has no instance variables and does not implement `Drop`.
    // - It is main-thread-only, as every object that handles AppKit actions
    //   and window delegate calls must be.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "XPackInstallerController"]
    pub(super) struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    // SAFETY: the one delegate method implemented has the signature AppKit
    // declares for it.
    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            with_app(App::close_requested);
            // The window is hidden when the wizard ends, never closed from
            // here: the run loop decides when that is.
            false
        }
    }

    // Every action method takes the sender AppKit passes and ignores it: the
    // model, not the control, knows what the action means.
    impl Controller {
        #[unsafe(method(back:))]
        fn back(&self, _sender: Option<&AnyObject>) {
            with_app(App::back);
        }

        #[unsafe(method(primary:))]
        fn primary(&self, _sender: Option<&AnyObject>) {
            with_app(App::primary);
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            with_app(App::close_requested);
        }

        #[unsafe(method(browse:))]
        fn browse(&self, _sender: Option<&AnyObject>) {
            with_app(App::browse);
        }

        #[unsafe(method(rootEdited:))]
        fn root_edited(&self, _sender: Option<&AnyObject>) {
            with_app(App::commit_root);
        }

        #[unsafe(method(retry:))]
        fn retry(&self, _sender: Option<&AnyObject>) {
            with_app(App::retry);
        }

        #[unsafe(method(launchExisting:))]
        fn launch_existing(&self, _sender: Option<&AnyObject>) {
            with_app(App::launch_existing);
        }

        #[unsafe(method(toggleAccepted:))]
        fn toggle_accepted(&self, sender: Option<&AnyObject>) {
            let on = is_on(sender);
            with_app(|app| app.toggle(|wizard| wizard.set_accepted(on)));
        }

        #[unsafe(method(toggleShortcut:))]
        fn toggle_shortcut(&self, sender: Option<&AnyObject>) {
            let on = is_on(sender);
            with_app(|app| app.toggle(|wizard| wizard.set_shortcut(on)));
        }

        #[unsafe(method(toggleLaunch:))]
        fn toggle_launch(&self, sender: Option<&AnyObject>) {
            let on = is_on(sender);
            with_app(|app| app.toggle(|wizard| wizard.set_launch(on)));
        }
    }
);

impl Controller {
    /// A controller that is never freed.
    ///
    /// Buttons do not retain their target, and `AppKit` may keep a view — and
    /// the buttons in it — alive in an autorelease pool after the wizard has
    /// let go of it. Keeping one extra reference for the life of the process
    /// costs one small object, and makes "the target outlives every button"
    /// true unconditionally rather than true given a drop order.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        #[allow(unsafe_code)]
        // SAFETY: `init` is NSObject's designated initialiser, and the class
        // adds no state that would need initialising first.
        let controller: Retained<Self> = unsafe { msg_send![super(this), init] };
        std::mem::forget(Retained::clone(&controller));
        controller
    }
}

/// Whether the checkbox that sent an action is now ticked.
pub(super) fn is_on(sender: Option<&AnyObject>) -> bool {
    sender
        .and_then(|sender| sender.downcast_ref::<NSButton>())
        .is_some_and(|button| button.state() == NSControlStateValueOn)
}

/// A push button that sends `action` to `target`.
pub(super) fn push_button(
    title: &str,
    target: &Controller,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    #[allow(unsafe_code)]
    // SAFETY: `target` is the controller, which defines `action` with the
    // `(sender)` signature AppKit calls it with, and which outlives every
    // button because it is never freed (see `Controller::new`).
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

/// A checkbox that sends `action` to `target` when toggled.
pub(super) fn checkbox(
    title: &str,
    on: bool,
    target: &Controller,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    #[allow(unsafe_code)]
    // SAFETY: as for `push_button`.
    let button = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    };
    button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    button
}

/// An editable text field that sends `action` to `target` when edited.
pub(super) fn text_field(
    value: &str,
    target: &Controller,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSTextField> {
    let field = NSTextField::textFieldWithString(&NSString::from_str(value), mtm);
    #[allow(unsafe_code)]
    // SAFETY: as for `push_button`.
    unsafe {
        field.setTarget(Some(target));
        field.setAction(Some(action));
    }
    field
}

/// The wizard's window.
pub(super) fn new_window(mtm: MainThreadMarker, title: &str) -> Retained<NSWindow> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    #[allow(unsafe_code)]
    // SAFETY: a freshly allocated window with a valid frame and style. It is
    // never closed, only hidden, so AppKit's release-on-close behaviour,
    // which would free it under the `Retained` that owns it, never applies.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(title));
    window
}

/// Starts or stops an indeterminate bar's animation.
pub(super) fn animate(bar: &NSProgressIndicator, on: bool) {
    #[allow(unsafe_code)]
    // SAFETY: a bar owned by the current view, on the main thread, with no
    // sender, which the method accepts.
    unsafe {
        if on {
            bar.startAnimation(None);
        } else {
            bar.stopAnimation(None);
        }
    }
}

/// The semibold weight, which `AppKit` exports as a constant.
pub(super) fn unsafe_semibold() -> objc2_app_kit::NSFontWeight {
    #[allow(unsafe_code)]
    // SAFETY: a constant AppKit declares and never changes.
    unsafe {
        NSFontWeightSemibold
    }
}

/// The run loop mode ordinary events arrive in.
pub(super) fn default_run_loop_mode() -> &'static objc2_foundation::NSRunLoopMode {
    #[allow(unsafe_code)]
    // SAFETY: a constant Foundation declares and never frees.
    unsafe {
        objc2_foundation::NSDefaultRunLoopMode
    }
}

/// The dark appearance, for drawing the wizard as a dark-mode user sees it.
pub(super) fn dark_appearance() -> Option<Retained<objc2_app_kit::NSAppearance>> {
    #[allow(unsafe_code)]
    // SAFETY: a constant `AppKit` declares and never frees.
    let name = unsafe { objc2_app_kit::NSAppearanceNameDarkAqua };
    objc2_app_kit::NSAppearance::appearanceNamed(name)
}
