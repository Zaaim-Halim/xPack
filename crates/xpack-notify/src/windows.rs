//! The Windows dialog.
//!
//! A window built from Win32 controls rather than a `MessageBox`, for one
//! reason that matters and one that follows from it.
//!
//! A `MessageBox` takes its button labels from Windows: "Yes" and "No", never
//! "Restart now" and "Later". A user reading a dialog about their application
//! closing should not have to work out what "Yes" is going to do. It also
//! cannot show the application's own icon, so the one dialog that claims to
//! speak for an application would be the one place it does not look like it.
//!
//! Everything here is a stock Win32 control, so the window inherits the
//! system's font, colours, scaling and screen-reader behaviour rather than
//! reimplementing any of it.

use std::cell::Cell;
use std::rc::Rc;

use native_windows_gui as nwg;

use crate::{Answer, Buttons, Prompt};

/// Window size in logical pixels, chosen to fit a two-line title and a short
/// paragraph without scrolling at the default font size.
const WINDOW: (i32, i32) = (460, 210);
/// Edge padding, matching what Windows' own dialogs leave.
const MARGIN: i32 = 16;
/// The icon is drawn at the size Explorer uses for a large icon.
const ICON: i32 = 48;

/// Opens the window and waits for the user to answer it.
pub(crate) fn show(prompt: &Prompt) -> Answer {
    if nwg::init().is_err() {
        return Answer::NotShown;
    }
    // The system font, rather than whatever the controls default to, which is
    // a bitmap font from a much older Windows.
    let mut font = nwg::Font::default();
    let _ = nwg::Font::builder().family("Segoe UI").size(16).build(&mut font);
    let mut heading = nwg::Font::default();
    let _ = nwg::Font::builder().family("Segoe UI").size(20).weight(600).build(&mut heading);

    // The application's own icon, where the installation has one. It titles
    // the window, appears on the task bar, and is drawn beside the message.
    let mut icon = nwg::Icon::default();
    let icon = prompt
        .icon
        .as_ref()
        .and_then(|path| path.to_str())
        .and_then(|path| nwg::Icon::builder().source_file(Some(path)).build(&mut icon).ok())
        .map(|()| &icon);

    let mut window = nwg::Window::default();
    if nwg::Window::builder()
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
        .size(WINDOW)
        .center(true)
        .title(&prompt.title())
        .icon(icon)
        .build(&mut window)
        .is_err()
    {
        return Answer::NotShown;
    }

    if icon.is_some() {
        let mut image = nwg::ImageFrame::default();
        let _ = nwg::ImageFrame::builder()
            .icon(icon)
            .size((ICON, ICON))
            .position((MARGIN, MARGIN))
            .parent(&window)
            .build(&mut image);
        // Deliberately leaked into the window's lifetime below; see `hold`.
        hold(Box::new(image));
    }

    let text_left = if icon.is_some() { MARGIN * 2 + ICON } else { MARGIN };
    let text_width = WINDOW.0 - text_left - MARGIN;

    let mut title = nwg::Label::default();
    let _ = nwg::Label::builder()
        .text(&prompt.title())
        .font(Some(&heading))
        .size((text_width, 24))
        .position((text_left, MARGIN))
        .parent(&window)
        .build(&mut title);

    let mut message = nwg::Label::default();
    let _ = nwg::Label::builder()
        .text(&prompt.message())
        .font(Some(&font))
        .size((text_width, 80))
        .position((text_left, MARGIN + 30))
        .parent(&window)
        .build(&mut message);

    let button_y = WINDOW.1 - 42 - MARGIN;
    let mut primary = nwg::Button::default();
    let mut secondary = nwg::Button::default();

    let (primary_text, has_secondary) = match prompt.buttons() {
        Buttons::Acknowledge => ("OK", false),
        Buttons::ApplyOrLater => ("Restart now", true),
    };

    if has_secondary {
        let _ = nwg::Button::builder()
            .text("Later")
            .font(Some(&font))
            .size((110, 32))
            .position((WINDOW.0 - MARGIN - 110, button_y))
            .parent(&window)
            .build(&mut secondary);
    }
    let primary_x =
        if has_secondary { WINDOW.0 - MARGIN - 110 - 8 - 120 } else { WINDOW.0 - MARGIN - 120 };
    let _ = nwg::Button::builder()
        .text(primary_text)
        .font(Some(&font))
        .size((120, 32))
        .position((primary_x, button_y))
        .parent(&window)
        .build(&mut primary);

    // Closing the window without choosing is a refusal, not an acceptance:
    // nothing should restart a user's application because they dismissed a
    // box they did not read.
    let answer = Rc::new(Cell::new(Answer::Later));
    let primary_handle = primary.handle;
    let chosen = Rc::clone(&answer);

    let handler =
        nwg::full_bind_event_handler(&window.handle, move |event, _data, handle| match event {
            nwg::Event::OnButtonClick => {
                if handle == primary_handle {
                    chosen.set(Answer::Apply);
                }
                nwg::stop_thread_dispatch();
            }
            nwg::Event::OnWindowClose => nwg::stop_thread_dispatch(),
            _ => {}
        });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);

    answer.get()
}

/// Keeps a control alive for the lifetime of the process.
///
/// A Win32 control is destroyed when its Rust handle is dropped, so one built
/// and then let go of would vanish from a window that is still showing it.
/// This process exists to display one dialog and exit, so holding them until
/// it does is the whole of the lifetime management required.
fn hold(control: Box<nwg::ImageFrame>) {
    Box::leak(control);
}
