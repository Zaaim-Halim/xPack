//! Every control the wizard can show, created once and laid out per page.
//!
//! `native-windows-gui` binds its event handler to the controls that exist
//! when it is bound, so controls are never created later: each render shows,
//! hides, moves and relabels the same set. Positions are logical pixels at
//! 96 DPI; the library scales them to the monitor.
//!
//! The layout follows Wizard97: the first and last pages have a banner down
//! the left, the pages between have a header across the top, and every page
//! ends in the button row.

use native_windows_gui as nwg;

use crate::model::{FinishView, Key, Page, Severity, Visibility, Wizard};

/// The window's client area.
const WIDTH: i32 = 600;
const HEIGHT: i32 = 430;
/// The banner on the first and last pages.
const BANNER: i32 = 164;
/// Where exterior pages put their text.
const EXTERIOR_X: i32 = BANNER + 20;
const EXTERIOR_WIDTH: i32 = WIDTH - EXTERIOR_X - 22;
/// Where interior pages put their body, under the header.
const BODY_X: i32 = 38;
const BODY_Y: i32 = 70;
const BODY_WIDTH: i32 = WIDTH - BODY_X - 22;
/// The button row.
const BUTTON_Y: i32 = HEIGHT - 46 + 11;
const BUTTON_WIDTH: i32 = 75;
const BUTTON_HEIGHT: i32 = 23;
const CANCEL_X: i32 = WIDTH - 12 - BUTTON_WIDTH;
const NEXT_X: i32 = CANCEL_X - 10 - BUTTON_WIDTH;
const BACK_X: i32 = NEXT_X - BUTTON_WIDTH;
/// How many lines of text the busiest page uses.
const LINES: usize = 12;

/// What a clicked control means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Back,
    Primary,
    Cancel,
    Browse,
    Retry,
    LaunchExisting,
    Accept,
    Shortcut,
    Launch,
}

/// The window and everything in it.
#[derive(Default)]
pub(super) struct Controls {
    pub(super) window: nwg::Window,
    pub(super) notice: nwg::Notice,
    normal: nwg::Font,
    bold: nwg::Font,
    large: nwg::Font,
    icon: nwg::Icon,
    picture: nwg::Bitmap,
    has_picture: bool,
    image: nwg::ImageFrame,
    heading: nwg::Label,
    subtitle: nwg::Label,
    lines: Vec<nwg::Label>,
    pub(super) licence: nwg::TextBox,
    pub(super) root: nwg::TextInput,
    browse: nwg::Button,
    status: nwg::Label,
    retry: nwg::Button,
    launch_existing: nwg::Button,
    pub(super) accept: nwg::CheckBox,
    pub(super) shortcut: nwg::CheckBox,
    pub(super) launch: nwg::CheckBox,
    progress: nwg::ProgressBar,
    back: nwg::Button,
    next: nwg::Button,
    cancel: nwg::Button,
}

impl Controls {
    /// Creates the window, hidden, and every control in it.
    pub(super) fn build(title: &str, icon: Option<&[u8]>) -> Result<Self, nwg::NwgError> {
        let mut c = Self::default();

        // The system font, rather than the bitmap one controls otherwise get.
        nwg::Font::builder().family("Segoe UI").size(16).build(&mut c.normal)?;
        nwg::Font::builder().family("Segoe UI").size(16).weight(700).build(&mut c.bold)?;
        nwg::Font::builder().family("Segoe UI").size(22).weight(700).build(&mut c.large)?;

        // The application's icon, read from its verified package. A format
        // Windows cannot load leaves the window with the default icon.
        let has_icon = icon.is_some_and(|bytes| {
            nwg::Icon::builder().source_bin(Some(bytes)).strict(true).build(&mut c.icon).is_ok()
        });
        c.has_picture = icon.is_some_and(|bytes| {
            nwg::Bitmap::builder()
                .source_bin(Some(bytes))
                .size(Some((96, 96)))
                .strict(true)
                .build(&mut c.picture)
                .is_ok()
        });

        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW)
            .size((WIDTH, HEIGHT))
            .center(true)
            .title(title)
            .icon(has_icon.then_some(&c.icon))
            .build(&mut c.window)?;
        nwg::Notice::builder().parent(&c.window).build(&mut c.notice)?;

        let window = &c.window;
        nwg::ImageFrame::builder()
            .bitmap(c.has_picture.then_some(&c.picture))
            .parent(window)
            .build(&mut c.image)?;
        label(window, &c.large, &mut c.heading)?;
        label(window, &c.normal, &mut c.subtitle)?;
        for _ in 0..LINES {
            let mut line = nwg::Label::default();
            label(window, &c.normal, &mut line)?;
            c.lines.push(line);
        }
        label(window, &c.normal, &mut c.status)?;

        nwg::TextBox::builder()
            .flags(nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL)
            .readonly(true)
            .font(Some(&c.normal))
            .parent(window)
            .build(&mut c.licence)?;
        nwg::TextInput::builder().font(Some(&c.normal)).parent(window).build(&mut c.root)?;
        nwg::ProgressBar::builder().range(0..1000).parent(window).build(&mut c.progress)?;

        for button in [
            &mut c.browse,
            &mut c.retry,
            &mut c.launch_existing,
            &mut c.back,
            &mut c.next,
            &mut c.cancel,
        ] {
            nwg::Button::builder().font(Some(&c.normal)).parent(window).build(button)?;
        }
        for checkbox in [&mut c.accept, &mut c.shortcut, &mut c.launch] {
            nwg::CheckBox::builder().font(Some(&c.normal)).parent(window).build(checkbox)?;
        }
        Ok(c)
    }

    /// What a clicked control means, if it is one of ours.
    pub(super) fn action_for(&self, handle: nwg::ControlHandle) -> Option<Action> {
        [
            (&self.back.handle, Action::Back),
            (&self.next.handle, Action::Primary),
            (&self.cancel.handle, Action::Cancel),
            (&self.browse.handle, Action::Browse),
            (&self.retry.handle, Action::Retry),
            (&self.launch_existing.handle, Action::LaunchExisting),
            (&self.accept.handle, Action::Accept),
            (&self.shortcut.handle, Action::Shortcut),
            (&self.launch.handle, Action::Launch),
        ]
        .into_iter()
        .find_map(|(candidate, action)| (*candidate == handle).then_some(action))
    }

    /// Lays the window out for the wizard as it is now.
    pub(super) fn render(&self, wizard: &Wizard) {
        self.hide_all();
        let texts = wizard.texts();
        self.window.set_text(&texts.line(Key::WindowTitle));

        let page = wizard.page();
        match page {
            Page::Welcome | Page::Finish => self.exterior(),
            _ => self.interior(wizard, page),
        }
        match page {
            Page::Welcome => self.welcome(wizard),
            Page::Licence => self.licence_page(wizard),
            Page::Location => self.location(wizard),
            Page::Ready => self.ready(wizard),
            Page::Installing => self.installing(wizard),
            Page::Finish => {
                if let Some(view) = wizard.finish_view() {
                    self.finish(wizard, &view);
                }
            }
        }
        self.buttons(wizard);
    }

    /// Updates the progress bar and its lines in place.
    pub(super) fn update_progress(&self, wizard: &Wizard) {
        let progress = wizard.progress();
        self.lines[0].set_text(progress.status());
        if let Some(permille) = progress.permille() {
            self.progress.set_marquee(false, 0);
            self.progress.remove_flags(nwg::ProgressBarFlags::MARQUEE);
            self.progress.set_pos(u32::from(permille));
        } else {
            // Indeterminate until the first count arrives, rather than
            // sitting at zero and looking stuck.
            self.progress.add_flags(nwg::ProgressBarFlags::MARQUEE);
            self.progress.set_marquee(true, 30);
        }
        self.lines[1].set_text(&progress.counts_line(wizard.texts()).unwrap_or_default());
    }

    // --- the frame of each page ---------------------------------------------------

    fn exterior(&self) {
        if self.has_picture {
            place(&self.image, (BANNER - 96) / 2, 40, 96, 96);
        }
    }

    fn interior(&self, wizard: &Wizard, page: Page) {
        let texts = wizard.texts();
        let (title, subtitle) = match page {
            Page::Licence => (Key::LicenceTitle, Key::LicenceSubtitle),
            Page::Location => (Key::LocationTitle, Key::LocationSubtitle),
            Page::Ready => (Key::ReadyTitle, Key::ReadySubtitle),
            _ => (Key::InstallingTitle, Key::InstallingSubtitle),
        };
        self.heading.set_font(Some(&self.bold));
        text(&self.heading, &texts.line(title), 16, 10, WIDTH - 90, 20);
        text(&self.subtitle, &texts.get(subtitle).unwrap_or_default(), 32, 32, WIDTH - 110, 20);
        if self.has_picture {
            place(&self.image, WIDTH - 56, 9, 40, 40);
        }
    }

    // --- the pages ------------------------------------------------------------------

    fn welcome(&self, wizard: &Wizard) {
        let texts = wizard.texts();
        self.heading.set_font(Some(&self.large));
        text(&self.heading, &texts.line(Key::WelcomeTitle), EXTERIOR_X, 18, EXTERIOR_WIDTH, 60);

        let mut y = 92;
        let mut next = self.lines.iter();
        let mut add = |line: String, height: i32| {
            if let Some(label) = next.next() {
                text(label, &line, EXTERIOR_X, y, EXTERIOR_WIDTH, height);
            }
            y += height + 6;
        };
        add(texts.line(Key::WelcomeHeadline), 20);
        if let Some(byline) = texts.get(Key::WelcomeByline) {
            add(byline, 20);
        }
        if let Some(description) = texts.facts().description.clone() {
            add(description, 40);
        }
        add(texts.line(Key::WelcomeVerified), 20);
        add(texts.line(Key::Welcome), 60);
        if let Some(hint) = texts.get(Key::WelcomeHint) {
            add(hint, 40);
        }
    }

    fn licence_page(&self, wizard: &Wizard) {
        let texts = wizard.texts();
        // Windows edit controls need CRLF line endings to break lines.
        let licence =
            wizard.licence().unwrap_or_default().replace("\r\n", "\n").replace('\n', "\r\n");
        self.licence.set_text(&licence);
        place(&self.licence, BODY_X, BODY_Y, BODY_WIDTH, 250);
        check(&self.accept, &texts.line(Key::LicenceAccept), wizard.accepted(), BODY_X, 330);
    }

    fn location(&self, wizard: &Wizard) {
        let texts = wizard.texts();
        let mut y = BODY_Y;
        text(&self.lines[0], &texts.line(Key::LocationPerUser), BODY_X, y, BODY_WIDTH, 36);
        y += 44;
        text(&self.lines[1], &texts.line(Key::LocationField), BODY_X, y, BODY_WIDTH, 20);
        y += 22;

        let field_width = BODY_WIDTH - 100;
        if self.root.text() != wizard.root().display().to_string() {
            self.root.set_text(&wizard.root().display().to_string());
        }
        self.root.set_readonly(!wizard.root_editable());
        place(&self.root, BODY_X, y, field_width, 23);
        let buttons = wizard.buttons();
        if buttons.browse != Visibility::Hidden {
            button(&self.browse, &texts.line(Key::Browse), BODY_X + field_width + 8, y, 92, true);
        }
        y += 34;

        if let Some(target) = wizard.target() {
            let line = format!("{} {}", texts.line(Key::LocationTarget), target.display());
            text(&self.lines[2], &line, BODY_X, y, BODY_WIDTH, 20);
            y += 30;
        }
        y = self.status(wizard, y);

        if wizard.shortcut_offered() {
            check(
                &self.shortcut,
                &texts.line(Key::LocationShortcut),
                wizard.shortcut(),
                BODY_X,
                y + 10,
            );
        }
    }

    /// The status line and the button beside it, from `y` down; returns where
    /// it stopped.
    fn status(&self, wizard: &Wizard, y: i32) -> i32 {
        let texts = wizard.texts();
        let status = wizard.status();
        let buttons = wizard.buttons();
        // Words carry the severity: no shield anywhere, because on Windows a
        // shield means administrator rights, the opposite of what this is.
        let prefix = match status.severity {
            Severity::Info => String::new(),
            Severity::Warning | Severity::Error => String::from("⚠  "),
        };
        let body = match &status.heading {
            Some(heading) => format!("{prefix}{heading} {}", status.body),
            None => format!("{prefix}{}", status.body),
        };
        let has_button =
            buttons.retry != Visibility::Hidden || buttons.launch_existing != Visibility::Hidden;
        let width = if has_button { BODY_WIDTH - 130 } else { BODY_WIDTH };
        text(&self.status, &body, BODY_X, y, width, 48);

        let side = BODY_X + BODY_WIDTH - 120;
        if buttons.retry != Visibility::Hidden {
            button(&self.retry, &texts.line(Key::Retry), side, y, 120, true);
        } else if buttons.launch_existing != Visibility::Hidden {
            button(&self.launch_existing, &texts.line(Key::Launch), side, y, 120, true);
        }
        y + 56
    }

    fn ready(&self, wizard: &Wizard) {
        let mut y = BODY_Y;
        let mut labels = self.lines.iter();
        for (name, value) in wizard.summary() {
            if let (Some(name_label), Some(value_label)) = (labels.next(), labels.next()) {
                text(name_label, &name, BODY_X, y, 140, 20);
                text(value_label, &value, BODY_X + 146, y, BODY_WIDTH - 146, 36);
            }
            y += 40;
        }
        if wizard.shows_status() {
            self.status(wizard, y + 6);
        } else if let Some(hint) = labels.next() {
            text(hint, &wizard.texts().line(Key::ReadyHint), BODY_X, 320, BODY_WIDTH, 36);
        }
    }

    fn installing(&self, wizard: &Wizard) {
        text(&self.lines[0], "", BODY_X, BODY_Y, BODY_WIDTH, 20);
        place(&self.progress, BODY_X, BODY_Y + 28, BODY_WIDTH, 20);
        text(&self.lines[1], "", BODY_X, BODY_Y + 56, BODY_WIDTH, 20);
        let note = wizard.texts().line(Key::InstallingNote);
        text(&self.lines[2], &note, BODY_X, 320, BODY_WIDTH, 36);
        self.update_progress(wizard);
    }

    fn finish(&self, wizard: &Wizard, view: &FinishView) {
        self.heading.set_font(Some(&self.large));
        text(&self.heading, &view.title, EXTERIOR_X, 18, EXTERIOR_WIDTH, 60);

        let mut y = 92;
        let mut next = self.lines.iter();
        let mut add = |line: &str, height: i32| {
            if let Some(label) = next.next() {
                text(label, line, EXTERIOR_X, y, EXTERIOR_WIDTH, height);
            }
            y += height + 8;
        };
        add(&view.body, 60);
        for line in [&view.location, &view.shortcut].into_iter().flatten() {
            add(line, 40);
        }
        if let Some(details) = &view.details {
            add(&wizard.texts().line(Key::FailedDetails), 20);
            add(details, 80);
        }
        if let Some(log) = &view.log {
            add(log, 40);
        }
        if let Some(launch) = &view.launch {
            check(&self.launch, launch, wizard.launch(), EXTERIOR_X, y + 4);
        }
    }

    fn buttons(&self, wizard: &Wizard) {
        let texts = wizard.texts();
        let buttons = wizard.buttons();
        let shown = |visibility| visibility != Visibility::Hidden;
        let enabled = |visibility| visibility == Visibility::Enabled;

        if shown(buttons.back) {
            button(
                &self.back,
                &texts.line(Key::Back),
                BACK_X,
                BUTTON_Y,
                BUTTON_WIDTH,
                enabled(buttons.back),
            );
        }
        // On the last page the row holds only Close, where Cancel was.
        let primary_x = if shown(buttons.cancel) { NEXT_X } else { CANCEL_X };
        button(
            &self.next,
            &buttons.primary_label,
            primary_x,
            BUTTON_Y,
            BUTTON_WIDTH,
            enabled(buttons.primary),
        );
        if shown(buttons.cancel) {
            button(
                &self.cancel,
                &texts.line(Key::Cancel),
                CANCEL_X,
                BUTTON_Y,
                BUTTON_WIDTH,
                enabled(buttons.cancel),
            );
        }
    }

    fn hide_all(&self) {
        self.image.set_visible(false);
        self.heading.set_visible(false);
        self.subtitle.set_visible(false);
        for line in &self.lines {
            line.set_visible(false);
        }
        self.status.set_visible(false);
        self.licence.set_visible(false);
        self.root.set_visible(false);
        self.progress.set_visible(false);
        for button in
            [&self.browse, &self.retry, &self.launch_existing, &self.back, &self.next, &self.cancel]
        {
            button.set_visible(false);
        }
        for checkbox in [&self.accept, &self.shortcut, &self.launch] {
            checkbox.set_visible(false);
        }
    }
}

// --- small helpers -----------------------------------------------------------------

/// A label that wraps onto as many lines as its height allows.
fn label(
    window: &nwg::Window,
    font: &nwg::Font,
    out: &mut nwg::Label,
) -> Result<(), nwg::NwgError> {
    // Top-aligned: a centred label is a single line, and would not wrap.
    nwg::Label::builder().v_align(nwg::VTextAlign::Top).font(Some(font)).parent(window).build(out)
}

/// Anything that can be moved, sized and shown.
trait Placeable {
    fn set_position(&self, x: i32, y: i32);
    fn set_size(&self, width: u32, height: u32);
    fn set_visible(&self, visible: bool);
}

macro_rules! placeable {
    ($($control:ty),*) => {$(
        impl Placeable for $control {
            fn set_position(&self, x: i32, y: i32) { <$control>::set_position(self, x, y) }
            fn set_size(&self, width: u32, height: u32) { <$control>::set_size(self, width, height) }
            fn set_visible(&self, visible: bool) { <$control>::set_visible(self, visible) }
        }
    )*};
}
placeable!(
    nwg::Label,
    nwg::Button,
    nwg::CheckBox,
    nwg::TextBox,
    nwg::TextInput,
    nwg::ProgressBar,
    nwg::ImageFrame
);

fn place(control: &impl Placeable, x: i32, y: i32, width: i32, height: i32) {
    control.set_position(x, y);
    control.set_size(width.unsigned_abs(), height.unsigned_abs());
    control.set_visible(true);
}

fn text(label: &nwg::Label, value: &str, x: i32, y: i32, width: i32, height: i32) {
    label.set_text(value);
    place(label, x, y, width, height);
}

fn button(button: &nwg::Button, title: &str, x: i32, y: i32, width: i32, enabled: bool) {
    button.set_text(title);
    button.set_enabled(enabled);
    place(button, x, y, width, BUTTON_HEIGHT);
}

fn check(checkbox: &nwg::CheckBox, title: &str, on: bool, x: i32, y: i32) {
    checkbox.set_text(title);
    checkbox.set_check_state(if on {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    });
    place(checkbox, x, y, BODY_WIDTH, 20);
}
