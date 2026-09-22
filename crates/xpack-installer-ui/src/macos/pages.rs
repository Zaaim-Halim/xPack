//! Drawing each page of the wizard from the model.
//!
//! Every view is built afresh on each render and positioned by hand, in the
//! window's own coordinates, where `y` grows upwards from the bottom edge.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSImageScaling, NSImageView, NSProgressIndicator,
    NSProgressIndicatorStyle, NSTextField, NSTextView, NSTitlePosition, NSView, NSWindowButton,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::controller::{animate, checkbox, push_button, text_field, unsafe_semibold};
use super::{
    App, BOX_BOTTOM, BOX_TOP, BUTTON_HEIGHT, BUTTON_WIDTH, HEIGHT, INSET, Live, MAIN_WIDTH, MAIN_X,
    MARGIN, SIDEBAR, WIDTH,
};
use crate::model::{CloseRequest, Key, Page, Severity, Visibility};

impl App {
    // --- drawing ------------------------------------------------------------------

    /// Rebuilds the window from the model.
    pub(super) fn render(&mut self) {
        let mtm = self.mtm;
        let backdrop = backdrop(mtm);
        let content = backdrop.contentView().expect("a box has a content view");
        self.root_field = None;
        self.live = None;

        self.draw_sidebar(&content);

        let texts = self.session.wizard().texts();
        let title = match self.session.wizard().page() {
            Page::Welcome => texts.line(Key::WelcomeTitle),
            Page::Licence => texts.line(Key::LicenceTitle),
            Page::Location => texts.line(Key::LocationTitle),
            Page::Ready => texts.line(Key::ReadyTitle),
            Page::Installing => texts.line(Key::InstallingTitle),
            Page::Finish => {
                self.session.wizard().finish_view().map(|view| view.title).unwrap_or_default()
            }
        };
        let heading = label(mtm, &title, MAIN_X, HEIGHT - MARGIN - 22.0, MAIN_WIDTH, 22.0);
        heading.setFont(Some(&NSFont::systemFontOfSize_weight(15.0, unsafe_semibold())));
        content.addSubview(&heading);

        let frame = content_box(mtm);
        content.addSubview(&frame);
        let inner = frame.contentView().expect("a box has a content view");
        match self.session.wizard().page() {
            Page::Welcome => self.draw_welcome(&inner),
            Page::Licence => self.draw_licence(&inner, &content),
            Page::Location => self.draw_location(&inner),
            Page::Ready => self.draw_ready(&inner),
            Page::Installing => self.draw_installing(&inner),
            Page::Finish => self.draw_finish(&inner, &content),
        }

        self.draw_buttons(&content);
        self.window.setContentView(Some(&backdrop));
    }

    fn draw_sidebar(&self, content: &NSView) {
        let mtm = self.mtm;
        let slot =
            NSRect::new(NSPoint::new(MARGIN, HEIGHT - MARGIN - 104.0), NSSize::new(SIDEBAR, 104.0));
        if let Some(icon) = &self.icon {
            let image = NSImageView::imageViewWithImage(icon, mtm);
            image.setFrame(slot);
            image.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            content.addSubview(&image);
        }

        let texts = self.session.wizard().texts();
        let current = self.session.wizard().page();
        let mut y = HEIGHT - MARGIN - 104.0 - 18.0 - 24.0;
        for page in self.session.wizard().pages().iter() {
            let (dot, color) = match page.cmp(&current) {
                std::cmp::Ordering::Equal => ("●", NSColor::controlAccentColor()),
                std::cmp::Ordering::Less => ("●", NSColor::secondaryLabelColor()),
                std::cmp::Ordering::Greater => ("○", NSColor::secondaryLabelColor()),
            };
            let marker = label(mtm, dot, MARGIN, y, 16.0, 20.0);
            marker.setTextColor(Some(&color));
            content.addSubview(&marker);

            let name =
                label(mtm, &texts.line(page.step_key()), MARGIN + 18.0, y, SIDEBAR - 18.0, 20.0);
            if page == current {
                name.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe_semibold())));
            } else {
                name.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            content.addSubview(&name);
            y -= 24.0;
        }
    }

    fn draw_welcome(&self, inner: &NSView) {
        let mtm = self.mtm;
        let texts = self.session.wizard().texts();
        let width = box_width();
        let mut y = box_height() - INSET;

        let mut add = |text: String, height: f64, bold: bool, secondary: bool| {
            y -= height;
            let line = wrapping(mtm, &text, INSET, y, width, height);
            if bold {
                line.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            }
            if secondary {
                line.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            inner.addSubview(&line);
            y -= 6.0;
        };
        add(texts.line(Key::WelcomeHeadline), 18.0, true, false);
        if let Some(byline) = texts.get(Key::WelcomeByline) {
            add(byline, 18.0, false, true);
        }
        if let Some(description) = texts.facts().description.clone() {
            add(description, 36.0, false, false);
        }
        add(texts.line(Key::WelcomeVerified), 18.0, false, true);
        add(texts.line(Key::Welcome), 54.0, false, false);
    }

    fn draw_licence(&self, inner: &NSView, content: &NSView) {
        let mtm = self.mtm;
        let scroll = NSTextView::scrollableTextView(mtm);
        scroll.setFrame(rect(INSET, INSET, box_width(), box_height() - 2.0 * INSET));
        if let Some(text) =
            scroll.documentView().and_then(|view| view.downcast::<NSTextView>().ok())
        {
            text.setString(&NSString::from_str(
                self.session.wizard().licence().unwrap_or_default(),
            ));
            text.setEditable(false);
            text.setSelectable(true);
        }
        inner.addSubview(&scroll);

        let accept = checkbox(
            &self.session.wizard().texts().line(Key::LicenceAccept),
            self.session.wizard().accepted(),
            &self.controller,
            sel!(toggleAccepted:),
            mtm,
        );
        accept.setFrame(rect(MAIN_X, BOX_BOTTOM - 30.0, MAIN_WIDTH, 22.0));
        content.addSubview(&accept);
    }

    fn draw_location(&mut self, inner: &NSView) {
        let mtm = self.mtm;
        let texts = self.session.wizard().texts();
        let width = box_width();
        let mut y = box_height() - INSET;

        y -= 36.0;
        let note = wrapping(mtm, &texts.line(Key::LocationPerUser), INSET, y, width, 36.0);
        note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        inner.addSubview(&note);

        y -= 26.0;
        inner.addSubview(&label(mtm, &texts.line(Key::LocationField), INSET, y, width, 18.0));
        y -= 28.0;
        let field_width = width - 90.0;
        let field = text_field(
            &self.session.wizard().root().display().to_string(),
            &self.controller,
            sel!(rootEdited:),
            mtm,
        );
        field.setFrame(rect(INSET, y, field_width, 24.0));
        field.setEditable(self.session.wizard().root_editable());
        inner.addSubview(&field);
        let buttons = self.session.wizard().buttons();
        if buttons.browse != Visibility::Hidden {
            let browse =
                push_button(&texts.line(Key::Browse), &self.controller, sel!(browse:), mtm);
            browse.setFrame(rect(INSET + field_width + 6.0, y - 2.0, 84.0, 28.0));
            inner.addSubview(&browse);
        }
        self.root_field = Some(field);

        if let Some(target) = self.session.wizard().target() {
            y -= 24.0;
            let line = format!("{} {}", texts.line(Key::LocationTarget), target.display());
            let target = label(mtm, &line, INSET, y, width, 18.0);
            target.setTextColor(Some(&NSColor::secondaryLabelColor()));
            inner.addSubview(&target);
        }

        y -= 12.0;
        y = self.draw_status(inner, y);

        if self.session.wizard().shortcut_offered() {
            let shortcut = checkbox(
                &texts.line(Key::LocationShortcut),
                self.session.wizard().shortcut(),
                &self.controller,
                sel!(toggleShortcut:),
                mtm,
            );
            shortcut.setFrame(rect(INSET, (y - 30.0).max(INSET), width, 22.0));
            inner.addSubview(&shortcut);
        }
    }

    /// Draws the status line and its buttons from `y` down; returns where it
    /// stopped.
    fn draw_status(&self, inner: &NSView, mut y: f64) -> f64 {
        let mtm = self.mtm;
        let status = self.session.wizard().status();
        let buttons = self.session.wizard().buttons();
        let width = box_width();
        let (symbol, color) = match status.severity {
            Severity::Info => ("ⓘ", NSColor::secondaryLabelColor()),
            Severity::Warning => ("⚠", NSColor::systemOrangeColor()),
            Severity::Error => ("✕", NSColor::systemRedColor()),
        };
        let has_button =
            buttons.retry != Visibility::Hidden || buttons.launch_existing != Visibility::Hidden;
        let text_width = if has_button { width - 120.0 } else { width } - 22.0;

        y -= 48.0;
        let icon = label(mtm, symbol, INSET, y + 30.0, 18.0, 18.0);
        icon.setTextColor(Some(&color));
        inner.addSubview(&icon);
        let text = match &status.heading {
            Some(heading) => format!("{heading} {}", status.body),
            None => status.body.clone(),
        };
        inner.addSubview(&wrapping(mtm, &text, INSET + 22.0, y, text_width, 48.0));

        let texts = self.session.wizard().texts();
        let side = |key, action| {
            let button = push_button(&texts.line(key), &self.controller, action, mtm);
            button.setFrame(rect(INSET + width - 112.0, y + 18.0, 112.0, 28.0));
            inner.addSubview(&button);
        };
        if buttons.retry != Visibility::Hidden {
            side(Key::Retry, sel!(retry:));
        } else if buttons.launch_existing != Visibility::Hidden {
            side(Key::Launch, sel!(launchExisting:));
        }
        y
    }

    fn draw_ready(&self, inner: &NSView) {
        let mtm = self.mtm;
        let mut y = box_height() - INSET;
        for (name, value) in self.session.wizard().summary() {
            y -= 36.0;
            let label_view = label(mtm, &name, INSET, y + 18.0, 110.0, 18.0);
            label_view.setTextColor(Some(&NSColor::secondaryLabelColor()));
            inner.addSubview(&label_view);
            inner.addSubview(&wrapping(mtm, &value, INSET + 116.0, y, box_width() - 116.0, 36.0));
        }
        if self.session.wizard().shows_status() {
            self.draw_status(inner, y - 8.0);
        } else {
            let hint = wrapping(
                mtm,
                &self.session.wizard().texts().line(Key::ReadyHint),
                INSET,
                INSET,
                box_width(),
                36.0,
            );
            hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
            inner.addSubview(&hint);
        }
    }

    fn draw_installing(&mut self, inner: &NSView) {
        let mtm = self.mtm;
        let width = box_width();
        let top = box_height() - INSET;

        let status =
            label(mtm, self.session.wizard().progress().status(), INSET, top - 20.0, width, 18.0);
        inner.addSubview(&status);

        let bar = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(mtm),
            rect(INSET, top - 52.0, width, 20.0),
        );
        bar.setStyle(NSProgressIndicatorStyle::Bar);
        bar.setMinValue(0.0);
        bar.setMaxValue(1000.0);
        inner.addSubview(&bar);

        let counts = label(mtm, "", INSET, top - 76.0, width, 18.0);
        counts.setTextColor(Some(&NSColor::secondaryLabelColor()));
        inner.addSubview(&counts);

        let note = wrapping(
            mtm,
            &self.session.wizard().texts().line(Key::InstallingNote),
            INSET,
            INSET,
            width,
            36.0,
        );
        note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        inner.addSubview(&note);

        // Indeterminate until the first count arrives; `update_live` switches
        // it over when one does.
        let determinate = self.session.wizard().progress().permille().is_some();
        bar.setIndeterminate(!determinate);
        if !determinate {
            animate(&bar, true);
        }
        self.live = Some(Live { status, bar, counts, determinate });
        self.update_live();
    }

    /// Updates the progress widgets in place.
    pub(super) fn update_live(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        let progress = self.session.wizard().progress();
        live.status.setStringValue(&NSString::from_str(progress.status()));
        if let Some(permille) = progress.permille() {
            if !live.determinate {
                animate(&live.bar, false);
                live.bar.setIndeterminate(false);
                live.determinate = true;
            }
            live.bar.setDoubleValue(f64::from(permille));
        }
        let counts = progress.counts_line(self.session.wizard().texts()).unwrap_or_default();
        live.counts.setStringValue(&NSString::from_str(&counts));
    }

    fn draw_finish(&self, inner: &NSView, content: &NSView) {
        let mtm = self.mtm;
        let Some(view) = self.session.wizard().finish_view() else {
            return;
        };
        let width = box_width();
        let mut y = box_height() - INSET;
        let mut add = |text: &str, height: f64, color: Option<Retained<NSColor>>| {
            y -= height;
            let line = wrapping(mtm, text, INSET, y, width, height);
            if let Some(color) = color {
                line.setTextColor(Some(&color));
            }
            inner.addSubview(&line);
            y -= 8.0;
        };

        if view.succeeded {
            add(&view.body, 36.0, None);
        } else {
            add(&format!("✕  {}", view.body), 54.0, Some(NSColor::systemRedColor()));
        }
        if let Some(location) = &view.location {
            add(location, 36.0, Some(NSColor::secondaryLabelColor()));
        }
        if let Some(shortcut) = &view.shortcut {
            add(shortcut, 18.0, Some(NSColor::secondaryLabelColor()));
        }
        if let Some(details) = &view.details {
            add(&self.session.wizard().texts().line(Key::FailedDetails), 18.0, None);
            add(details, 72.0, Some(NSColor::secondaryLabelColor()));
        }
        if let Some(log) = &view.log {
            add(log, 36.0, Some(NSColor::secondaryLabelColor()));
        }

        if let Some(launch) = &view.launch {
            let button = checkbox(
                launch,
                self.session.wizard().launch(),
                &self.controller,
                sel!(toggleLaunch:),
                mtm,
            );
            button.setFrame(rect(MAIN_X, BOX_BOTTOM - 30.0, MAIN_WIDTH, 22.0));
            content.addSubview(&button);
        }
    }

    fn draw_buttons(&self, content: &NSView) {
        let mtm = self.mtm;
        let texts = self.session.wizard().texts();
        let buttons = self.session.wizard().buttons();

        let primary = push_button(&buttons.primary_label, &self.controller, sel!(primary:), mtm);
        primary.setFrame(rect(WIDTH - MARGIN - BUTTON_WIDTH, MARGIN, BUTTON_WIDTH, BUTTON_HEIGHT));
        primary.setEnabled(buttons.primary == Visibility::Enabled);
        primary.setKeyEquivalent(&NSString::from_str("\r"));
        content.addSubview(&primary);

        if buttons.back != Visibility::Hidden {
            let back = push_button(&texts.line(Key::Back), &self.controller, sel!(back:), mtm);
            back.setFrame(rect(
                WIDTH - MARGIN - 2.0 * BUTTON_WIDTH - 10.0,
                MARGIN,
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
            ));
            back.setEnabled(buttons.back == Visibility::Enabled);
            content.addSubview(&back);
        }

        if buttons.cancel != Visibility::Hidden {
            let cancel =
                push_button(&texts.line(Key::Cancel), &self.controller, sel!(cancel:), mtm);
            cancel.setFrame(rect(MAIN_X, MARGIN, 90.0, BUTTON_HEIGHT));
            cancel.setEnabled(buttons.cancel == Visibility::Enabled);
            cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
            content.addSubview(&cancel);
        }

        // The close button follows the same rule as Cancel: unusable while an
        // installation runs.
        if let Some(close) = self.window.standardWindowButton(NSWindowButton::CloseButton) {
            close.setEnabled(self.session.wizard().request_close() != CloseRequest::Refuse);
        }
    }
}

// --- small helpers -------------------------------------------------------------

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// A one-line label.
fn label(
    mtm: MainThreadMarker,
    text: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(rect(x, y, width, height));
    label
}

/// A label that wraps onto as many lines as its height allows.
fn wrapping(
    mtm: MainThreadMarker,
    text: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setFrame(rect(x, y, width, height));
    label
}

/// The window's root view: the window background, painted.
///
/// A plain view is transparent and relies on the window to paint behind it.
/// Painting it here gives the same result on screen and makes the view
/// complete on its own, so an image of it is what the window shows.
fn backdrop(mtm: MainThreadMarker) -> Retained<NSBox> {
    let backdrop = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, WIDTH, HEIGHT));
    backdrop.setBoxType(NSBoxType::Custom);
    backdrop.setTitlePosition(NSTitlePosition::NoTitle);
    backdrop.setBorderWidth(0.0);
    backdrop.setCornerRadius(0.0);
    backdrop.setFillColor(&NSColor::windowBackgroundColor());
    backdrop.setContentViewMargins(NSSize::new(0.0, 0.0));
    backdrop
}

/// The rounded box pages draw into.
fn content_box(mtm: MainThreadMarker) -> Retained<NSBox> {
    let frame = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        rect(MAIN_X, BOX_BOTTOM, MAIN_WIDTH, BOX_TOP - BOX_BOTTOM),
    );
    frame.setBoxType(NSBoxType::Custom);
    frame.setTitlePosition(NSTitlePosition::NoTitle);
    frame.setCornerRadius(7.0);
    frame.setFillColor(&NSColor::controlBackgroundColor());
    frame.setBorderColor(&NSColor::separatorColor());
    frame.setContentViewMargins(NSSize::new(0.0, 0.0));
    frame
}

fn box_width() -> f64 {
    MAIN_WIDTH - 2.0 * INSET
}

fn box_height() -> f64 {
    BOX_TOP - BOX_BOTTOM
}
