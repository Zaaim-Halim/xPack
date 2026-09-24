//! The wizard: which page is showing, what each button does, and when the
//! window may close.
//!
//! A front-end draws [`Wizard::buttons`] and the page's text, and reports what
//! the person did. Every rule about what is allowed lives here, including the
//! ones a window gets wrong most easily: closing while an installation runs,
//! going back past it, or offering an install the installer will refuse.

use std::path::{Path, PathBuf};

use xpack_core::ProgressEvent;
use xpack_install::Existing;

use super::page::{Page, PageSet};
use super::plan::UiPlan;
use super::progress::Progress;
use super::status::{InstallKind, Severity, Status};
use super::text::{Facts, Flavour, Key, Texts};
use crate::engine::{Choices, Failure, FailureKind, Inspection, Installed};

/// Everything a wizard is built from.
#[derive(Debug, Clone)]
pub struct WizardSpec {
    /// Which platform's conventions to follow.
    pub flavour: Flavour,
    /// What is being installed, from the verified manifest.
    pub facts: Facts,
    /// The publisher's settings. Every field has a default.
    pub plan: UiPlan,
    /// The licence text. Without it there is no licence page, whatever the
    /// page list says.
    pub licence: Option<String>,
    /// Whether the signed manifest asks for a desktop entry.
    pub shortcut_requested: bool,
    /// The command the signed manifest names, if any, and whether its name is
    /// free to take.
    pub command: Option<CommandOffer>,
    /// Where to install: the root the console installer would use.
    pub root: PathBuf,
    /// Whether the root is fixed because the application is already
    /// installed there. A second copy elsewhere would fight the first over
    /// the same menu entry and uninstall entry.
    pub root_fixed: bool,
    /// Where this run is logged, for the failure page to name.
    pub log: Option<PathBuf>,
}

/// The commands a package names, as the wizard offers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOffer {
    /// What the person would type: the main command first.
    pub names: Vec<String>,
    /// Those another program already owns where they would go. They are left
    /// out, and said so: the installer never replaces what is not its own.
    pub taken: Vec<String>,
}

impl CommandOffer {
    /// The names that will be added, when the box is ticked.
    fn free(&self) -> Vec<&str> {
        self.names.iter().filter(|name| !self.taken.contains(name)).map(String::as_str).collect()
    }
}

/// `“a”`, `“a” and “b”`, `“a”, “b” and “c”`.
fn quoted_list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("\u{201c}{name}\u{201d}")).collect();
    match quoted.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Whether a control is drawn, and whether it can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Not drawn.
    Hidden,
    /// Drawn, and cannot be used.
    Disabled,
    /// Drawn, and can be used.
    Enabled,
}

impl Visibility {
    fn enabled_if(condition: bool) -> Self {
        if condition { Self::Enabled } else { Self::Disabled }
    }

    fn shown_if(condition: bool) -> Self {
        if condition { Self::Enabled } else { Self::Hidden }
    }
}

/// The controls a front-end draws, as they should be right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buttons {
    /// Back.
    pub back: Visibility,
    /// The primary button: Next, Install or Close. Always the default button.
    pub primary: Visibility,
    /// What the primary button says.
    pub primary_label: String,
    /// Cancel.
    pub cancel: Visibility,
    /// Browse, beside the location field.
    pub browse: Visibility,
    /// Retry, beside a status that is worth retrying.
    pub retry: Visibility,
    /// Launch, beside a status saying this version is already installed.
    pub launch_existing: Visibility,
}

/// What happens when the person closes the window or presses Cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRequest {
    /// Close now.
    Close,
    /// Ask first; close only on [`Wizard::confirm_cancel`].
    Confirm,
    /// Refuse: an installation is running and cannot be stopped.
    Refuse,
}

/// How a step forward went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Now showing this page.
    Moved(Page),
    /// Now installing: run [`crate::Engine::install`] with these choices, on a
    /// worker thread, and report the result to [`Wizard::finished`].
    Install(Choices),
    /// Nothing happened: the button was not enabled.
    Refused,
}

/// How the run ended, for the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    /// Installed.
    Installed,
    /// Tried to install, and failed.
    Failed(FailureKind),
    /// The person cancelled before anything was changed.
    Cancelled,
    /// This version was already installed; the person opened it or closed.
    AlreadyInstalled,
}

/// The last page, filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishView {
    /// Whether this is a success.
    pub succeeded: bool,
    /// Which icon, for a failure.
    pub severity: Severity,
    /// The heading.
    pub title: String,
    /// The paragraph under it.
    pub body: String,
    /// Where it was installed, on success.
    pub location: Option<String>,
    /// That a desktop entry was added, when one was.
    pub shortcut: Option<String>,
    /// How to start the application from a terminal, when the command was
    /// added, and what to do when a terminal would not find it yet.
    pub command: Vec<String>,
    /// The engine's own message, on failure.
    pub details: Option<String>,
    /// Where the log is, on failure, when there is one.
    pub log: Option<String>,
    /// The launch checkbox's label, when it is offered.
    pub launch: Option<String>,
}

/// Where the run is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Choosing,
    Installing,
    Finished(Result<Installed, Failure>),
    Ended(Conclusion),
}

/// What the person has chosen so far.
///
/// One `bool` per checkbox: each is an independent yes or no, and nothing
/// combines them into states an enum would name.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
struct Selections {
    accepted: bool,
    shortcut: bool,
    command: bool,
    launch: bool,
}

/// The wizard.
#[derive(Debug, Clone)]
pub struct Wizard {
    texts: Texts,
    pages: PageSet,
    page: Page,
    phase: Phase,
    licence: Option<String>,
    shortcut_requested: bool,
    command_offer: Option<CommandOffer>,
    launch_on_finish: bool,
    root: PathBuf,
    root_fixed: bool,
    inspection: Option<Inspection>,
    selections: Selections,
    progress: Progress,
    log: Option<PathBuf>,
}

impl Wizard {
    /// A wizard on its first page.
    ///
    /// The first inspection has not happened yet: ask
    /// [`Self::pending_inspection`] and report the answer.
    pub fn new(spec: WizardSpec) -> Self {
        let pages = spec.plan.page_set_given(spec.licence.is_some());
        let texts = Texts::new(spec.flavour, spec.facts, spec.plan.text.clone());
        Self {
            texts,
            page: pages.first(),
            pages,
            phase: Phase::Choosing,
            licence: spec.licence,
            shortcut_requested: spec.shortcut_requested,
            command_offer: spec.command,
            launch_on_finish: spec.plan.launch_on_finish,
            root: spec.root,
            root_fixed: spec.root_fixed,
            inspection: None,
            selections: Selections {
                accepted: false,
                shortcut: spec.plan.shortcut_default,
                command: spec.plan.path_default,
                // Ticked when offered: the publisher offering it is the
                // reason it is there.
                launch: true,
            },
            progress: Progress::default(),
            log: spec.log,
        }
    }

    // --- reading ------------------------------------------------------------

    /// The page showing.
    pub fn page(&self) -> Page {
        self.page
    }

    /// The pages this wizard shows, for a step list.
    pub fn pages(&self) -> &PageSet {
        &self.pages
    }

    /// Every line of text, for this platform and this application.
    pub fn texts(&self) -> &Texts {
        &self.texts
    }

    /// The licence text, when there is a licence page.
    pub fn licence(&self) -> Option<&str> {
        self.licence.as_deref()
    }

    /// Whether the licence has been accepted.
    pub fn accepted(&self) -> bool {
        self.selections.accepted
    }

    /// The root the location field shows.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the location can be changed.
    pub fn root_editable(&self) -> bool {
        !self.root_fixed && self.is_choosing()
    }

    /// Where the application will live, once inspected.
    pub fn target(&self) -> Option<&Path> {
        self.inspection.as_ref().map(|inspection| inspection.target.as_path())
    }

    /// The status line for the current root.
    ///
    /// Shown on the location page, and on the page that starts the
    /// installation whenever it blocks, so a refusal is never unexplained.
    pub fn status(&self) -> Status {
        Status::of(self.inspection.as_ref(), &self.texts)
    }

    /// Whether the status line belongs on the current page.
    pub fn shows_status(&self) -> bool {
        self.page == Page::Location
            || (self.page == self.pages.last_before_install() && !self.status().allows_install)
    }

    /// What installing will be, once that is known and allowed.
    pub fn install_kind(&self) -> Option<InstallKind> {
        match &self.inspection.as_ref()?.verdict {
            Ok(existing) => InstallKind::of(existing),
            Err(_) => None,
        }
    }

    /// Whether the desktop-entry box is shown.
    ///
    /// Only when the package asks for an entry, and only on a first
    /// installation: an existing one keeps the updater it was installed with,
    /// which would put a declined entry back.
    pub fn shortcut_offered(&self) -> bool {
        self.shortcut_requested
            && matches!(
                self.inspection.as_ref().map(|inspection| &inspection.verdict),
                Some(Ok(existing)) if existing.is_first_install()
            )
    }

    /// Whether the desktop-entry box is ticked.
    pub fn shortcut(&self) -> bool {
        self.selections.shortcut
    }

    /// Whether the command box is drawn, and whether it can be ticked.
    ///
    /// Hidden unless the package names a command, and on the same terms as
    /// the shortcut box: only on a first installation. Drawn but unusable
    /// when another program owns every name, so the person sees why nothing
    /// will be added rather than wondering where it went.
    pub fn command_visibility(&self) -> Visibility {
        match &self.command_offer {
            Some(offer) if self.is_first_install() => {
                Visibility::enabled_if(!offer.free().is_empty())
            }
            _ => Visibility::Hidden,
        }
    }

    /// The command box's label, when it is drawn: every name it offers.
    pub fn command_label(&self) -> Option<String> {
        let offer = self.command_offer.as_ref()?;
        let names: Vec<&str> = offer.names.iter().map(String::as_str).collect();
        (self.command_visibility() != Visibility::Hidden).then(|| {
            self.texts.line_with(Key::LocationCommand, &[("command", &quoted_list(&names))])
        })
    }

    /// Which names are left out because another program owns them, when any
    /// are.
    pub fn command_taken_note(&self) -> Option<String> {
        let offer = self.command_offer.as_ref()?;
        let taken: Vec<&str> = offer.taken.iter().map(String::as_str).collect();
        (self.command_visibility() != Visibility::Hidden && !taken.is_empty()).then(|| {
            self.texts.line_with(Key::LocationCommandTaken, &[("command", &quoted_list(&taken))])
        })
    }

    /// Whether the command box is ticked. Never, when it cannot be used.
    pub fn command(&self) -> bool {
        self.selections.command && self.command_visibility() == Visibility::Enabled
    }

    fn is_first_install(&self) -> bool {
        matches!(
            self.inspection.as_ref().map(|inspection| &inspection.verdict),
            Some(Ok(existing)) if existing.is_first_install()
        )
    }

    /// The installation's progress.
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Whether the launch box is ticked.
    pub fn launch(&self) -> bool {
        self.selections.launch
    }

    /// How the run ended, or `None` while it has not.
    pub fn conclusion(&self) -> Option<Conclusion> {
        match &self.phase {
            Phase::Ended(conclusion) => Some(*conclusion),
            _ => None,
        }
    }

    // --- the buttons --------------------------------------------------------

    /// The controls, as they should be drawn now.
    pub fn buttons(&self) -> Buttons {
        let texts = &self.texts;
        match &self.phase {
            Phase::Choosing => {
                let status = self.status();
                let starts_install = self.page == self.pages.last_before_install();
                let label = if starts_install { Key::Install } else { Key::Next };
                let status_shown = self.shows_status();
                Buttons {
                    back: Visibility::enabled_if(self.pages.before(self.page).is_some()),
                    primary: Visibility::enabled_if(self.can_advance()),
                    primary_label: texts.line(label),
                    cancel: Visibility::Enabled,
                    browse: if self.page == Page::Location {
                        Visibility::shown_if(!self.root_fixed)
                    } else {
                        Visibility::Hidden
                    },
                    retry: Visibility::shown_if(status_shown && status.offers_retry),
                    launch_existing: Visibility::shown_if(status_shown && status.offers_launch),
                }
            }
            Phase::Installing => Buttons {
                back: Visibility::Disabled,
                primary: Visibility::Disabled,
                primary_label: texts.line(Key::Next),
                cancel: Visibility::Disabled,
                browse: Visibility::Hidden,
                retry: Visibility::Hidden,
                launch_existing: Visibility::Hidden,
            },
            Phase::Finished(_) | Phase::Ended(_) => Buttons {
                back: Visibility::Hidden,
                primary: Visibility::Enabled,
                primary_label: texts.line(Key::Close),
                cancel: Visibility::Hidden,
                browse: Visibility::Hidden,
                retry: Visibility::Hidden,
                launch_existing: Visibility::Hidden,
            },
        }
    }

    /// Whether the primary button may be pressed on this page.
    fn can_advance(&self) -> bool {
        if !self.is_choosing() {
            return false;
        }
        let licence_ok = self.page != Page::Licence || self.selections.accepted;
        let needs_a_verdict =
            self.page == Page::Location || self.page == self.pages.last_before_install();
        licence_ok && (!needs_a_verdict || self.status().allows_install)
    }

    fn is_choosing(&self) -> bool {
        self.phase == Phase::Choosing
    }

    // --- what the person does -----------------------------------------------

    /// The primary button on a page before the installation.
    pub fn advance(&mut self) -> Step {
        if !self.can_advance() {
            return Step::Refused;
        }
        match self.pages.after(self.page) {
            Some(Page::Installing) => {
                self.page = Page::Installing;
                self.phase = Phase::Installing;
                Step::Install(self.choices())
            }
            Some(page) => {
                self.page = page;
                Step::Moved(page)
            }
            None => Step::Refused,
        }
    }

    /// Back. Choices already made are kept.
    pub fn back(&mut self) -> Option<Page> {
        if !self.is_choosing() {
            return None;
        }
        let previous = self.pages.before(self.page)?;
        self.page = previous;
        Some(previous)
    }

    /// The licence box.
    pub fn set_accepted(&mut self, accepted: bool) {
        if self.is_choosing() {
            self.selections.accepted = accepted;
        }
    }

    /// The desktop-entry box. Ignored where it is not offered.
    pub fn set_shortcut(&mut self, wanted: bool) {
        if self.is_choosing() && self.shortcut_offered() {
            self.selections.shortcut = wanted;
        }
    }

    /// The command box. Ignored where it cannot be used.
    pub fn set_command(&mut self, wanted: bool) {
        if self.is_choosing() && self.command_visibility() == Visibility::Enabled {
            self.selections.command = wanted;
        }
    }

    /// The launch box on the last page.
    pub fn set_launch(&mut self, wanted: bool) {
        self.selections.launch = wanted;
    }

    /// A new root, typed or browsed to.
    ///
    /// Returns `false`, and changes nothing, when the root cannot be changed.
    /// Otherwise the previous verdict is dropped until the new root has been
    /// inspected, so nothing is ever allowed on the strength of another
    /// folder's answer.
    pub fn set_root(&mut self, root: PathBuf) -> bool {
        if !self.root_editable() {
            return false;
        }
        if root != self.root {
            self.root = root;
            self.inspection = None;
        }
        true
    }

    /// The root waiting to be inspected, if one is.
    pub fn pending_inspection(&self) -> Option<&Path> {
        if self.is_choosing() && self.inspection.is_none() { Some(&self.root) } else { None }
    }

    /// The answer to an inspection of `root`.
    ///
    /// Ignored when the root has changed since it was asked for: an answer
    /// that arrives late describes a folder nobody is looking at any more.
    pub fn inspected(&mut self, root: &Path, inspection: Inspection) {
        if self.is_choosing() && root == self.root {
            self.inspection = Some(inspection);
        }
    }

    /// Retry, beside a busy installation: look again.
    pub fn retry(&mut self) {
        if self.is_choosing() && self.status().offers_retry {
            self.inspection = None;
        }
    }

    /// Launch, beside "already installed": what to start, if anything.
    ///
    /// Ends the run. Nothing was installed, and nothing needed to be.
    pub fn launch_existing(&mut self) -> Option<PathBuf> {
        if !(self.is_choosing() && self.shows_status() && self.status().offers_launch) {
            return None;
        }
        self.phase = Phase::Ended(Conclusion::AlreadyInstalled);
        Some(self.root.clone())
    }

    /// Something the engine reported while installing.
    pub fn observe(&mut self, event: &ProgressEvent) {
        if self.phase == Phase::Installing {
            self.progress.observe(event);
        }
    }

    /// The installation's result.
    ///
    /// A busy installation goes back to the page that started it, with the
    /// reason and a Retry beside it: another operation got there first, and
    /// nothing was changed. Anything else is the last page.
    pub fn finished(&mut self, result: Result<Installed, Failure>) {
        if self.phase != Phase::Installing {
            return;
        }
        if let Err(Failure { kind: FailureKind::Busy, .. }) = &result {
            self.phase = Phase::Choosing;
            self.progress = Progress::default();
            self.page = self.pages.last_before_install();
            if let Some(inspection) = &mut self.inspection {
                inspection.verdict = Ok(Existing::Busy);
            }
            return;
        }
        self.page = Page::Finish;
        self.phase = Phase::Finished(result);
    }

    /// Cancel. What happens is the same as closing the window.
    pub fn request_cancel(&self) -> CloseRequest {
        self.request_close()
    }

    /// The window's close button.
    pub fn request_close(&self) -> CloseRequest {
        match self.phase {
            Phase::Choosing => CloseRequest::Confirm,
            Phase::Installing => CloseRequest::Refuse,
            Phase::Finished(_) | Phase::Ended(_) => CloseRequest::Close,
        }
    }

    /// The person confirmed they want to leave, before anything was changed.
    pub fn confirm_cancel(&mut self) {
        if self.is_choosing() {
            self.phase = Phase::Ended(Conclusion::Cancelled);
        }
    }

    /// Close on the last page: what to launch, if the person asked.
    ///
    /// A launch that then fails is reported, but does not change how the run
    /// ended: the installation itself succeeded.
    pub fn close(&mut self) -> Option<PathBuf> {
        let Phase::Finished(result) = &self.phase else {
            return None;
        };
        let (conclusion, launch) = match result {
            Ok(_) => (
                Conclusion::Installed,
                (self.launch_on_finish && self.selections.launch).then(|| self.root.clone()),
            ),
            Err(failure) => (Conclusion::Failed(failure.kind), None),
        };
        self.phase = Phase::Ended(conclusion);
        launch
    }

    // --- page content -------------------------------------------------------

    /// The choices an installation starts with.
    ///
    /// A declined entry is the only choice passed on. Everything else is
    /// `None`, which is what the installer does when nobody is asked.
    fn choices(&self) -> Choices {
        let declined = self.shortcut_offered() && !self.selections.shortcut;
        // A taken name is not a decline: nothing was asked, and the installer
        // leaves another program's command alone by itself.
        let command_declined =
            self.command_visibility() == Visibility::Enabled && !self.selections.command;
        Choices {
            root: self.root.clone(),
            desktop_entry: declined.then_some(false),
            command: command_declined.then_some(false),
        }
    }

    /// The summary rows on the Ready page: label, then value.
    pub fn summary(&self) -> Vec<(String, String)> {
        let texts = &self.texts;
        let mut rows = Vec::new();
        if let Some(target) = self.target() {
            rows.push((texts.line(Key::ReadyLocationLabel), target.display().to_string()));
        }
        if let Some(kind) = self.install_kind() {
            let value = match kind {
                InstallKind::New => texts.line(Key::ReadyKindNew),
                InstallKind::Upgrade(old) => {
                    texts.line_with(Key::ReadyKindUpgrade, &[("old", &old.to_string())])
                }
                InstallKind::Repair => texts.line(Key::ReadyKindRepair),
            };
            rows.push((texts.line(Key::ReadyKindLabel), value));
        }
        if self.shortcut_offered() {
            let value = if self.selections.shortcut {
                texts.line(Key::ReadyShortcutYes)
            } else {
                texts.line(Key::ReadyShortcutNo)
            };
            rows.push((texts.line(Key::ReadyShortcutLabel), value));
        }
        if let Some(offer) = &self.command_offer
            && self.command_visibility() != Visibility::Hidden
        {
            let value = if self.command() {
                texts.line_with(Key::ReadyCommandYes, &[("command", &quoted_list(&offer.free()))])
            } else {
                texts.line(Key::ReadyCommandNo)
            };
            rows.push((texts.line(Key::ReadyCommandLabel), value));
        }
        rows.push((texts.line(Key::ReadyAccountLabel), texts.line(Key::ReadyAccountValue)));
        rows
    }

    /// The last page, or `None` before it.
    pub fn finish_view(&self) -> Option<FinishView> {
        let texts = &self.texts;
        let Phase::Finished(result) = &self.phase else {
            return None;
        };
        Some(match result {
            Ok(installed) => FinishView {
                succeeded: true,
                severity: Severity::Info,
                title: texts.line(Key::FinishTitle),
                body: texts.line(Key::Finish),
                location: Some(texts.line_with(
                    Key::FinishLocation,
                    &[("path", &installed.directory.display().to_string())],
                )),
                shortcut: installed.shortcut_added.then(|| texts.line(Key::FinishShortcut)),
                command: command_lines(texts, installed),
                details: None,
                log: None,
                launch: self.launch_on_finish.then(|| texts.line(Key::Launch)),
            },
            Err(failure) => {
                let integrity = failure.kind == FailureKind::Integrity;
                let (title, body) = if integrity {
                    (Key::DamagedTitle, Key::DamagedBody)
                } else {
                    (Key::FailedTitle, Key::FailedBody)
                };
                FinishView {
                    succeeded: false,
                    severity: Severity::Error,
                    title: texts.line(title),
                    body: texts.line(body),
                    location: None,
                    shortcut: None,
                    command: Vec::new(),
                    details: Some(failure.message.clone()),
                    log: self.log.as_ref().map(|log| {
                        texts.line_with(Key::FailedLog, &[("log", &log.display().to_string())])
                    }),
                    launch: None,
                }
            }
        })
    }
}

/// The finish page's lines about the command, when one was added.
fn command_lines(texts: &Texts, installed: &Installed) -> Vec<String> {
    let Some(name) = &installed.command else {
        return Vec::new();
    };
    let mut lines = vec![texts.line_with(Key::FinishCommand, &[("command", name)])];
    if let Some(dir) = &installed.command_off_path {
        lines.push(texts.line_with(
            Key::FinishCommandOffPath,
            &[("command", name), ("path", &dir.display().to_string())],
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use xpack_core::Version;

    use super::*;
    use crate::engine::RootProblem;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn spec() -> WizardSpec {
        WizardSpec {
            flavour: Flavour::Windows,
            facts: Facts {
                name: "App".into(),
                version: v("2.0.0"),
                publisher: Some("Example".into()),
                description: None,
            },
            plan: UiPlan::default(),
            licence: Some("Terms.".into()),
            shortcut_requested: true,
            command: None,
            root: PathBuf::from("/home/u/apps"),
            root_fixed: false,
            log: Some(PathBuf::from("/tmp/install.log")),
        }
    }

    fn inspection(verdict: Result<Existing, RootProblem>) -> Inspection {
        Inspection { target: PathBuf::from("/home/u/apps/com.example.app"), verdict }
    }

    /// A wizard whose root has been inspected with this verdict.
    fn wizard_with(spec: WizardSpec, verdict: Result<Existing, RootProblem>) -> Wizard {
        let mut wizard = Wizard::new(spec);
        let root = wizard.pending_inspection().expect("a first inspection").to_path_buf();
        wizard.inspected(&root, inspection(verdict));
        wizard
    }

    /// Walks forward to the page, accepting the licence on the way.
    fn advance_to(wizard: &mut Wizard, page: Page) {
        while wizard.page() != page {
            if wizard.page() == Page::Licence {
                wizard.set_accepted(true);
            }
            assert!(matches!(wizard.advance(), Step::Moved(_)), "stuck on {:?}", wizard.page());
        }
    }

    /// Runs up to the start of the installation, returning the choices.
    fn start_install(wizard: &mut Wizard) -> Choices {
        advance_to(wizard, Page::Ready);
        match wizard.advance() {
            Step::Install(choices) => choices,
            other => panic!("expected the install to start, got {other:?}"),
        }
    }

    fn installed() -> Installed {
        Installed {
            directory: PathBuf::from("/home/u/apps/com.example.app"),
            version: v("2.0.0"),
            shortcut_added: true,
            command: None,
            command_off_path: None,
        }
    }

    fn failure(kind: FailureKind) -> Failure {
        Failure { kind, message: "it broke".into() }
    }

    // --- the first page -------------------------------------------------------

    #[test]
    fn the_first_page_cannot_go_back_but_can_go_on_or_be_left() {
        let wizard = wizard_with(spec(), Ok(Existing::Nothing));
        let buttons = wizard.buttons();
        assert_eq!(wizard.page(), Page::Welcome);
        assert_eq!(buttons.back, Visibility::Disabled, "disabled, not hidden");
        assert_eq!(buttons.primary, Visibility::Enabled);
        assert_eq!(buttons.primary_label, "Next >");
        assert_eq!(buttons.cancel, Visibility::Enabled);
        assert_eq!(wizard.request_close(), CloseRequest::Confirm);
    }

    // --- the licence ----------------------------------------------------------

    #[test]
    fn the_licence_must_be_accepted_to_go_on() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        wizard.advance();
        assert_eq!(wizard.page(), Page::Licence);
        assert!(!wizard.accepted(), "the box starts unticked");
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
        assert_eq!(wizard.advance(), Step::Refused);
        assert_eq!(wizard.page(), Page::Licence);

        wizard.set_accepted(true);
        assert_eq!(wizard.advance(), Step::Moved(Page::Location));
    }

    #[test]
    fn going_back_keeps_the_licence_accepted() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.back(), Some(Page::Licence));
        assert!(wizard.accepted());
        assert_eq!(wizard.buttons().primary, Visibility::Enabled);
    }

    #[test]
    fn with_no_licence_there_is_no_licence_page_even_when_listed() {
        let mut plain = spec();
        plain.licence = None;
        plain.plan.pages = Some(vec![Page::Welcome, Page::Licence, Page::Ready]);
        let wizard = wizard_with(plain, Ok(Existing::Nothing));
        assert!(!wizard.pages().contains(Page::Licence));
    }

    // --- the location ---------------------------------------------------------

    #[test]
    fn the_location_waits_for_its_inspection() {
        let mut wizard = Wizard::new(spec());
        advance_to_without_inspection(&mut wizard);
        assert_eq!(wizard.status().body, "Checking this folder…");
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
    }

    fn advance_to_without_inspection(wizard: &mut Wizard) {
        wizard.advance();
        wizard.set_accepted(true);
        wizard.advance();
        assert_eq!(wizard.page(), Page::Location);
    }

    #[test]
    fn a_new_root_drops_the_old_verdict_until_it_is_inspected() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert!(wizard.set_root(PathBuf::from("/elsewhere")));
        assert_eq!(wizard.pending_inspection(), Some(Path::new("/elsewhere")));
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
    }

    #[test]
    fn a_late_answer_about_a_previous_root_is_ignored() {
        let mut wizard = Wizard::new(spec());
        advance_to_without_inspection(&mut wizard);
        wizard.set_root(PathBuf::from("/second"));
        // The answer for the first root arrives after the person moved on.
        wizard.inspected(Path::new("/home/u/apps"), inspection(Ok(Existing::Nothing)));
        assert_eq!(wizard.pending_inspection(), Some(Path::new("/second")));
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
    }

    #[test]
    fn every_verdict_the_installer_refuses_blocks_the_next_button() {
        let refused = [
            Ok(Existing::Installed),
            Ok(Existing::Newer(v("3.0.0"))),
            Ok(Existing::Busy),
            Ok(Existing::Unreadable("corrupt".into())),
            Err(RootProblem::NotAbsolute),
            Err(RootProblem::NotWritable),
            Err(RootProblem::TooDeep),
        ];
        for verdict in refused {
            let mut wizard = wizard_with(spec(), verdict.clone());
            advance_to(&mut wizard, Page::Location);
            assert_eq!(wizard.buttons().primary, Visibility::Disabled, "{verdict:?}");
            assert_eq!(wizard.advance(), Step::Refused, "{verdict:?}");
        }
    }

    #[test]
    fn a_busy_installation_can_be_looked_at_again() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Busy));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.buttons().retry, Visibility::Enabled);
        wizard.retry();
        assert!(wizard.pending_inspection().is_some(), "retry asks again");
    }

    #[test]
    fn this_version_already_installed_offers_to_open_it_and_ends() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Installed));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.buttons().launch_existing, Visibility::Enabled);
        assert_eq!(wizard.launch_existing(), Some(PathBuf::from("/home/u/apps")));
        assert_eq!(wizard.conclusion(), Some(Conclusion::AlreadyInstalled));
    }

    #[test]
    fn a_fixed_root_cannot_be_changed() {
        let mut fixed = spec();
        fixed.root_fixed = true;
        let mut wizard = wizard_with(fixed, Ok(Existing::Older(v("1.0.0"))));
        advance_to(&mut wizard, Page::Location);
        assert!(!wizard.root_editable());
        assert!(!wizard.set_root(PathBuf::from("/elsewhere")));
        assert_eq!(wizard.root(), Path::new("/home/u/apps"));
        assert_eq!(wizard.buttons().browse, Visibility::Hidden);
    }

    // --- the desktop entry ----------------------------------------------------

    #[test]
    fn declining_the_entry_on_a_first_install_is_passed_on() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert!(wizard.shortcut_offered());
        assert!(wizard.shortcut(), "ticked by default");
        wizard.set_shortcut(false);
        assert_eq!(start_install(&mut wizard).desktop_entry, Some(false));
    }

    #[test]
    fn keeping_the_entry_passes_nothing_on() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        assert_eq!(start_install(&mut wizard).desktop_entry, None);
    }

    #[test]
    fn the_entry_is_never_offered_on_an_existing_installation() {
        // Its updater may predate the recorded choice and put the entry back.
        let mut wizard = wizard_with(spec(), Ok(Existing::Older(v("1.0.0"))));
        advance_to(&mut wizard, Page::Location);
        assert!(!wizard.shortcut_offered());
        wizard.set_shortcut(false);
        assert_eq!(start_install(&mut wizard).desktop_entry, None);
    }

    #[test]
    fn the_entry_is_not_offered_when_the_package_does_not_ask_for_one() {
        let mut none = spec();
        none.shortcut_requested = false;
        let wizard = wizard_with(none, Ok(Existing::Nothing));
        assert!(!wizard.shortcut_offered());
    }

    // --- the command box -------------------------------------------------------

    fn commanding(taken: bool) -> WizardSpec {
        let names = vec!["mytool".to_string()];
        let taken = if taken { names.clone() } else { Vec::new() };
        WizardSpec { command: Some(CommandOffer { names, taken }), ..spec() }
    }

    #[test]
    fn the_command_box_is_offered_ticked_and_can_be_declined() {
        let mut wizard = wizard_with(commanding(false), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.command_visibility(), Visibility::Enabled);
        assert_eq!(
            wizard.command_label().as_deref(),
            Some("Add \u{201c}mytool\u{201d} to the command line")
        );
        assert!(wizard.command(), "ticked by default");
        wizard.set_command(false);
        assert_eq!(start_install(&mut wizard).command, Some(false));
    }

    fn offering(names: &[&str], taken: &[&str]) -> WizardSpec {
        WizardSpec {
            command: Some(CommandOffer {
                names: names.iter().map(|name| (*name).to_string()).collect(),
                taken: taken.iter().map(|name| (*name).to_string()).collect(),
            }),
            ..spec()
        }
    }

    #[test]
    fn several_commands_share_one_box_that_names_them_all() {
        let mut wizard =
            wizard_with(offering(&["xpack", "cargo-xpack"], &[]), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(
            wizard.command_label().as_deref(),
            Some("Add \u{201c}xpack\u{201d} and \u{201c}cargo-xpack\u{201d} to the command line")
        );
        assert_eq!(wizard.command_taken_note(), None);
    }

    #[test]
    fn a_taken_name_is_left_out_and_said_while_the_others_are_still_offered() {
        let mut wizard = wizard_with(
            offering(&["xpack", "cargo-xpack"], &["cargo-xpack"]),
            Ok(Existing::Nothing),
        );
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.command_visibility(), Visibility::Enabled);
        assert!(wizard.command());
        let note = wizard.command_taken_note().unwrap();
        assert!(note.contains("cargo-xpack") && !note.contains("\u{201c}xpack"), "{note}");

        advance_to(&mut wizard, Page::Ready);
        let row = wizard.summary().into_iter().find(|(label, _)| label == "Command line");
        assert_eq!(row.map(|r| r.1).as_deref(), Some("Adds \u{201c}xpack\u{201d}"));
    }

    #[test]
    fn every_name_taken_leaves_the_box_unusable() {
        let mut wizard = wizard_with(
            offering(&["xpack", "cargo-xpack"], &["xpack", "cargo-xpack"]),
            Ok(Existing::Nothing),
        );
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.command_visibility(), Visibility::Disabled);
        assert!(!wizard.command());
    }

    #[test]
    fn keeping_the_command_passes_nothing_on() {
        let mut wizard = wizard_with(commanding(false), Ok(Existing::Nothing));
        assert_eq!(start_install(&mut wizard).command, None);
    }

    #[test]
    fn a_publisher_can_start_the_command_box_unticked() {
        let mut spec = commanding(false);
        spec.plan.path_default = false;
        let mut wizard = wizard_with(spec, Ok(Existing::Nothing));
        assert!(!wizard.command());
        assert_eq!(start_install(&mut wizard).command, Some(false));
    }

    #[test]
    fn a_name_someone_else_owns_is_shown_unusable_with_the_reason() {
        let mut wizard = wizard_with(commanding(true), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        assert_eq!(wizard.command_visibility(), Visibility::Disabled);
        assert!(!wizard.command(), "shown ticked while it cannot happen");
        assert!(wizard.command_taken_note().unwrap().contains("mytool"));
        wizard.set_command(true);
        assert!(!wizard.command(), "a box that cannot be used was ticked");
        // Not a decline: nothing was asked.
        assert_eq!(start_install(&mut wizard).command, None);
    }

    #[test]
    fn the_command_box_is_hidden_without_a_command_and_on_an_existing_installation() {
        let wizard = wizard_with(spec(), Ok(Existing::Nothing));
        assert_eq!(wizard.command_visibility(), Visibility::Hidden);
        assert_eq!(wizard.command_label(), None);

        let mut wizard = wizard_with(commanding(false), Ok(Existing::Older(v("1.0.0"))));
        assert_eq!(wizard.command_visibility(), Visibility::Hidden);
        wizard.set_command(false);
        assert_eq!(start_install(&mut wizard).command, None);
    }

    #[test]
    fn the_summary_says_whether_the_command_is_added() {
        let mut wizard = wizard_with(commanding(false), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Ready);
        let row = |wizard: &Wizard| {
            wizard.summary().into_iter().find(|(label, _)| label == "Command line").map(|r| r.1)
        };
        assert_eq!(row(&wizard).as_deref(), Some("Adds \u{201c}mytool\u{201d}"));

        let mut wizard = wizard_with(commanding(false), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Location);
        wizard.set_command(false);
        advance_to(&mut wizard, Page::Ready);
        assert_eq!(row(&wizard).as_deref(), Some("Not added"));
    }

    #[test]
    fn the_last_page_says_how_to_start_it_from_a_terminal() {
        let mut wizard = wizard_with(commanding(false), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Ok(Installed {
            command: Some("mytool".into()),
            command_off_path: Some(PathBuf::from("/home/u/.local/bin")),
            ..installed()
        }));
        let lines = wizard.finish_view().unwrap().command;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("Type mytool in a new terminal window"), "{lines:?}");
        assert!(lines[1].contains("/home/u/.local/bin is not on your PATH"), "{lines:?}");
    }

    #[test]
    fn a_command_that_was_not_added_is_not_described() {
        let mut wizard = wizard_with(commanding(true), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Ok(installed()));
        assert!(wizard.finish_view().unwrap().command.is_empty());
    }

    // --- ready and installing -------------------------------------------------

    #[test]
    fn the_page_before_installing_says_install() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Ready);
        assert_eq!(wizard.buttons().primary_label, "Install");
    }

    #[test]
    fn the_summary_describes_an_upgrade() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Older(v("1.4.2"))));
        advance_to(&mut wizard, Page::Ready);
        let summary = wizard.summary();
        assert!(summary.contains(&("Installation".into(), "Upgrade from 1.4.2 to 2.0.0".into())));
        assert!(summary.iter().all(|(label, _)| label != "Start Menu"), "not offered, not shown");
    }

    #[test]
    fn nothing_can_be_done_while_installing() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        let buttons = wizard.buttons();
        assert_eq!(wizard.page(), Page::Installing);
        assert_eq!(buttons.back, Visibility::Disabled);
        assert_eq!(buttons.primary, Visibility::Disabled);
        assert_eq!(buttons.cancel, Visibility::Disabled);
        assert_eq!(wizard.request_close(), CloseRequest::Refuse);
        assert_eq!(wizard.request_cancel(), CloseRequest::Refuse);
        assert_eq!(wizard.back(), None);
        assert_eq!(wizard.advance(), Step::Refused);
        wizard.confirm_cancel();
        assert_eq!(wizard.conclusion(), None, "an install cannot be cancelled");
        assert!(!wizard.set_root(PathBuf::from("/elsewhere")));
    }

    #[test]
    fn progress_is_taken_only_while_installing() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        let event = ProgressEvent::Installing { version: v("2.0.0") };
        wizard.observe(&event);
        assert_eq!(wizard.progress().status(), "", "not installing yet");
        start_install(&mut wizard);
        wizard.observe(&event);
        assert_eq!(wizard.progress().status(), "Installing 2.0.0");
    }

    #[test]
    fn with_no_location_page_the_install_button_still_waits_for_a_verdict() {
        let mut short = spec();
        short.plan.pages = Some(vec![Page::Welcome, Page::Ready]);
        let mut wizard = wizard_with(short, Ok(Existing::Newer(v("3.0.0"))));
        advance_to(&mut wizard, Page::Ready);
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
        assert!(wizard.shows_status(), "the refusal is explained where it happens");
    }

    // --- the result -----------------------------------------------------------

    #[test]
    fn a_busy_install_goes_back_with_the_reason_and_a_retry() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Err(failure(FailureKind::Busy)));
        assert_eq!(wizard.page(), Page::Ready);
        assert_eq!(wizard.status().heading.as_deref(), Some("App is busy."));
        assert_eq!(wizard.buttons().retry, Visibility::Enabled);
        assert_eq!(wizard.buttons().primary, Visibility::Disabled);
    }

    #[test]
    fn success_offers_nothing_to_launch_unless_the_publisher_asked() {
        // Off by default: the installer without a window starts nothing.
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Ok(installed()));
        let view = wizard.finish_view().unwrap();
        assert!(view.succeeded);
        assert_eq!(view.launch, None);
        assert_eq!(wizard.buttons().primary_label, "Close");
        assert_eq!(wizard.buttons().back, Visibility::Hidden);
        assert_eq!(wizard.request_close(), CloseRequest::Close);
        assert_eq!(wizard.close(), None);
        assert_eq!(wizard.conclusion(), Some(Conclusion::Installed));
    }

    #[test]
    fn success_launches_on_close_when_offered_and_ticked() {
        let mut launching = spec();
        launching.plan.launch_on_finish = true;
        let mut wizard = wizard_with(launching.clone(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Ok(installed()));
        assert_eq!(wizard.finish_view().unwrap().launch.as_deref(), Some("Launch App"));
        assert_eq!(wizard.close(), Some(PathBuf::from("/home/u/apps")));

        let mut declined = wizard_with(launching, Ok(Existing::Nothing));
        start_install(&mut declined);
        declined.finished(Ok(installed()));
        declined.set_launch(false);
        assert_eq!(declined.close(), None);
    }

    #[test]
    fn a_failure_shows_the_message_and_the_log() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Err(failure(FailureKind::Other)));
        let view = wizard.finish_view().unwrap();
        assert!(!view.succeeded);
        assert_eq!(view.title, "Setup could not install App");
        assert_eq!(view.details.as_deref(), Some("it broke"));
        assert_eq!(
            view.log.as_deref(),
            Some("A log of this attempt was written to /tmp/install.log")
        );
        wizard.close();
        assert_eq!(wizard.conclusion(), Some(Conclusion::Failed(FailureKind::Other)));
    }

    #[test]
    fn a_security_failure_says_the_installer_is_damaged() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        start_install(&mut wizard);
        wizard.finished(Err(failure(FailureKind::Integrity)));
        let view = wizard.finish_view().unwrap();
        assert_eq!(view.title, "This installer is damaged and can't be used.");
        wizard.close();
        assert_eq!(wizard.conclusion(), Some(Conclusion::Failed(FailureKind::Integrity)));
    }

    // --- leaving --------------------------------------------------------------

    #[test]
    fn cancelling_before_installing_asks_then_ends() {
        let mut wizard = wizard_with(spec(), Ok(Existing::Nothing));
        advance_to(&mut wizard, Page::Ready);
        assert_eq!(wizard.request_cancel(), CloseRequest::Confirm);
        assert_eq!(wizard.conclusion(), None, "asking is not leaving");
        wizard.confirm_cancel();
        assert_eq!(wizard.conclusion(), Some(Conclusion::Cancelled));
        assert_eq!(wizard.advance(), Step::Refused, "a cancelled wizard installs nothing");
    }

    #[test]
    fn leaving_every_setting_out_installs_exactly_as_the_console_would() {
        // The recommended wizard clicked straight through: the root the
        // console would use, and no choice passed on.
        let mut plain = spec();
        plain.licence = None;
        let mut wizard = wizard_with(plain, Ok(Existing::Nothing));
        let choices = start_install(&mut wizard);
        assert_eq!(
            choices,
            Choices { root: PathBuf::from("/home/u/apps"), desktop_entry: None, command: None }
        );
    }
}
