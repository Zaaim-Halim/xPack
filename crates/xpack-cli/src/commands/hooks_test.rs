//! `xpack hooks test`: a package's hooks run as users' machines will run
//! them, in throwaway installations, and a report of how they did.
//!
//! The scenarios, in an order that lets "nothing left behind" mean
//! something: this package installed in installation A; with `--previous`,
//! that release installed in installation B and this one applied over it,
//! confirmed, and undone by request; then A uninstalled, last. What the
//! hooks do is recorded by `xpack-hook`; in plan mode, the default, none of
//! it is done.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use xpack_core::hooks::{HookPoint, Moment, Operation, PlanAction, PlanLine, PointOutcome};
use xpack_core::progress::{ProgressEvent, ProgressReporter};
use xpack_core::{
    Error, InstallPaths, InstallScope, Manifest, Platform, Result, Sha256Digest, Version,
};
use xpack_install::{
    HookContext, HookTest, InstallOptions, Installer, TrustDecision, open_and_verify,
};
use xpack_platform::InstallLock;

/// The report `xpack hooks test` writes, and the release gate reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    /// The package file tested.
    pub(crate) package: String,
    /// The SHA-256 of its signed manifest: what binds this report to that
    /// exact build.
    pub(crate) manifest_sha256: Sha256Digest,
    /// The application and version.
    pub(crate) application: String,
    pub(crate) version: Version,
    /// The xPack release that ran the test.
    pub(crate) xpack: String,
    /// `plan` or `real`.
    pub(crate) mode: String,
    /// Who the installations were for: `user` or `machine`.
    pub(crate) scope: String,
    /// Whether every hook point that ran succeeded and held to its moment
    /// and permissions, and nothing was left behind.
    pub(crate) passed: bool,
    /// Each scenario: whether it ran, and why not.
    pub(crate) scenarios: Vec<Scenario>,
    /// Each hook point the package declares.
    pub(crate) points: Vec<PointReport>,
    /// Files the install and update hooks wrote outside the installation
    /// and nothing removed.
    pub(crate) left_behind: Vec<PathBuf>,
    /// Every program a hook ran: what was done through it cannot be seen.
    pub(crate) programs: Vec<String>,
}

/// One scenario of the test.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scenario {
    pub(crate) name: String,
    pub(crate) ran: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// One hook point's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PointReport {
    pub(crate) point: String,
    /// `succeeded`, `failed`, `interrupted`, or `not run`.
    pub(crate) result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// Whether the installation was as the moment promises when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) moment_held: Option<bool>,
    /// What its hooks did, or would have done, or were refused.
    pub(crate) actions: Vec<PlanAction>,
}

/// What the test is asked to do.
pub(crate) struct Test<'a> {
    /// The package as it can be read: itself, or its opened copy when it is
    /// sealed.
    pub(crate) package: &'a Path,
    /// The package as it was named.
    pub(crate) named: &'a Path,
    pub(crate) previous: Option<&'a Path>,
    pub(crate) real: bool,
    pub(crate) answers: BTreeMap<String, xpack_core::hooks::ProgramAnswer>,
    pub(crate) scope: InstallScope,
    pub(crate) engine: PathBuf,
}

/// The package's manifest, verified against the key it declares, and the
/// SHA-256 of its signed bytes: what a report is bound to.
pub(crate) fn package_identity(package: &Path) -> Result<(Manifest, Sha256Digest)> {
    let mut reader = xpack_package::PackageReader::open(package)?;
    let key = reader.declared_signing_key_unverified()?;
    let verified = reader.verify_with_keys(&[key])?;
    let digest = xpack_security::hash::sha256(verified.manifest_bytes());
    Ok((verified.manifest().clone(), digest))
}

/// Where a package's report for installations of `scope` is written, and
/// looked for: beside it, one per scope, as each scope has permissions of
/// its own.
pub(crate) fn report_path(package: &Path, scope: InstallScope) -> PathBuf {
    match scope {
        InstallScope::User => package.with_extension("hooks-report.json"),
        InstallScope::Machine => package.with_extension("hooks-report.all-users.json"),
    }
}

/// Runs the test and returns its report.
pub(crate) fn run(test: &Test<'_>) -> Result<Report> {
    let (manifest, digest) = package_identity(test.package)?;
    let host = Platform::host()?;
    if manifest.platform != host {
        return Err(Error::invalid(
            "hooks test",
            format!(
                "{} is a {} package; its hooks are tested on {}, where they will run",
                test.named.display(),
                manifest.platform,
                manifest.platform
            ),
        ));
    }
    let work = tempfile::tempdir().map_err(|e| Error::io(Path::new("a temporary directory"), e))?;
    // Canonical from the start, as `xpack-hook` records paths, and as the
    // installations cannot be once they are gone.
    let base = work.path().canonicalize().map_err(|e| Error::io(work.path(), e))?;
    let hook_test = HookTest {
        record: base.join("plan.jsonl"),
        perform: test.real,
        answers: test.answers.clone(),
    };
    let version = manifest.application.version.clone();
    let observer = Observer::new(version.clone());
    let a = InstallPaths::new(&base.join("a"), &manifest.application.id)?;
    let b = InstallPaths::new(&base.join("b"), &manifest.application.id)?;
    let mut scenarios = Vec::new();

    // A: this package, installed.
    observer.watch(&a, None);
    let installed = install(test, &a, test.package, &hook_test, &observer);
    scenarios.push(Scenario { name: "install".into(), ran: true, note: error_note(&installed) });

    // B: the previous release, then this package over it.
    match test.previous {
        None => scenarios.push(Scenario {
            name: "update".into(),
            ran: false,
            note: Some("no --previous release was given".into()),
        }),
        Some(previous) => {
            let (previous_manifest, _) = package_identity(previous)?;
            let from = previous_manifest.application.version.clone();
            observer.watch(&b, Some(from.clone()));
            if let Err(error) = install(test, &b, previous, &hook_test, &observer) {
                scenarios.push(Scenario {
                    name: "update".into(),
                    ran: false,
                    note: Some(format!("the previous release {from} did not install: {error}")),
                });
            } else {
                let updated = update_confirm_and_roll_back(test, &b, &hook_test, &observer);
                scenarios.push(Scenario {
                    name: "update".into(),
                    ran: true,
                    note: error_note(&updated),
                });
            }
        }
    }

    // A again, last: what the install and update hooks left must be gone
    // by the time the uninstall hooks finish.
    if installed.is_ok() {
        observer.watch(&a, None);
        let lock = InstallLock::acquire(&a)?;
        let removed =
            xpack_install::uninstall_under_test(lock, None, None, &observer, Some(&hook_test))
                .map(drop);
        scenarios.push(Scenario {
            name: "uninstall".into(),
            ran: true,
            note: error_note(&removed),
        });
    } else {
        scenarios.push(Scenario {
            name: "uninstall".into(),
            ran: false,
            note: Some("the install failed, and was undone".into()),
        });
    }

    let lines = read_plan(&hook_test.record, &version);
    let outcomes = outcomes(&lines);
    Ok(assemble(test, &manifest, digest, scenarios, &lines, &outcomes, &observer, &[&a, &b]))
}

/// Installs `package` into `paths`, its hooks under test.
fn install(
    test: &Test<'_>,
    paths: &InstallPaths,
    package: &Path,
    hook_test: &HookTest,
    observer: &Observer,
) -> Result<()> {
    let lock = InstallLock::acquire(paths)?;
    let mut verified = open_and_verify(package, &lock, &TrustDecision::OnFirstUse)?;
    let options = InstallOptions {
        activate: true,
        scope: test.scope,
        hook_engine: Some(test.engine.clone()),
        hook_test: Some(hook_test.clone()),
        ..Default::default()
    };
    let result = Installer::new(&lock).install_with_progress(&mut verified, &options, observer);
    drop(lock);
    if result.is_err() {
        let _ = xpack_install::finish_removal(paths);
    }
    result.map(drop)
}

/// This package applied over the previous one in `paths`, confirmed as a
/// good start confirms it, then undone by request.
fn update_confirm_and_roll_back(
    test: &Test<'_>,
    paths: &InstallPaths,
    hook_test: &HookTest,
    observer: &Observer,
) -> Result<()> {
    install(test, paths, test.package, hook_test, observer)?;
    let lock = InstallLock::acquire(paths)?;
    let installer = Installer::new(&lock);
    let hooks = HookContext {
        engine: Some(&test.engine),
        scope: test.scope,
        progress: observer,
        cancel: None,
        test: Some(hook_test),
    };
    // An installation for everyone confirmed it already, as it applied it.
    if test.scope == InstallScope::User
        && let Some(confirmation) = installer.commit_for_confirmation()?
    {
        confirmation.run(paths, &hooks);
    }
    installer.roll_back_on_request(&hooks).map(drop)
}

fn error_note(result: &Result<()>) -> Option<String> {
    result.as_ref().err().map(ToString::to_string)
}

/// The plan's lines for `version`'s own hooks: the previous release's are
/// its own business.
fn read_plan(record: &Path, version: &Version) -> Vec<PlanLine> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<PlanLine>(line).ok())
        .filter(|line| &line.version == version)
        .collect()
}

/// How each hook point ended, and why one failed: from the record, where
/// what ran each hook wrote it as it ended, so it is known even once the
/// installation that ran it is gone. A point succeeds only if every hook of
/// it did.
fn outcomes(lines: &[PlanLine]) -> BTreeMap<HookPoint, (PointOutcome, Option<String>)> {
    let mut found: BTreeMap<HookPoint, (PointOutcome, Option<String>)> = BTreeMap::new();
    for line in lines {
        let PlanAction::Ended { succeeded, reason } = &line.action else { continue };
        let entry = found.entry(line.point).or_insert((PointOutcome::Succeeded, None));
        if !succeeded {
            *entry = (PointOutcome::Failed, reason.clone());
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    test: &Test<'_>,
    manifest: &Manifest,
    digest: Sha256Digest,
    scenarios: Vec<Scenario>,
    lines: &[PlanLine],
    outcomes: &BTreeMap<HookPoint, (PointOutcome, Option<String>)>,
    observer: &Observer,
    roots: &[&InstallPaths],
) -> Report {
    let declared: BTreeSet<HookPoint> = manifest.hooks.all().map(|(point, _)| point).collect();
    let update_started = outcomes.keys().any(|point| point.operation == Operation::Update);
    let moments = observer.held();
    let mut points = Vec::new();
    let mut passed = true;
    for point in &declared {
        let actions: Vec<PlanAction> = lines
            .iter()
            .filter(|line| line.point == *point)
            .filter(|line| !matches!(line.action, PlanAction::Ended { .. }))
            .map(|line| line.action.clone())
            .collect();
        let refusal = actions.iter().find_map(|action| match action {
            PlanAction::Refused { reason } => Some(reason.clone()),
            _ => None,
        });
        let refused = refusal.is_some();
        let (result, mut reason) = match outcomes.get(point) {
            Some((PointOutcome::Succeeded, _)) => ("succeeded", None),
            Some((PointOutcome::Failed, why)) => ("failed", why.clone()),
            Some((PointOutcome::Interrupted, _)) => ("interrupted", None),
            None => {
                ("not run", Some(not_run_because(*point, update_started, test.previous.is_some())))
            }
        };
        if let Some(refusal) = refusal {
            reason = Some(format!("its permissions do not allow it: {refusal}"));
        }
        let moment_held = moments.get(point).copied();
        let ran = outcomes.contains_key(point);
        if (ran && result != "succeeded") || refused || moment_held == Some(false) {
            passed = false;
        }
        points.push(PointReport {
            point: point.to_string(),
            result: if refused && result == "succeeded" { "refused".into() } else { result.into() },
            reason,
            moment_held,
            actions,
        });
    }
    let left_behind = left_behind(lines, roots, test.real);
    if !left_behind.is_empty() {
        passed = false;
    }
    let programs: BTreeSet<String> = lines
        .iter()
        .filter_map(|line| match &line.action {
            PlanAction::Exec { program, args } => {
                Some(format!("{program} {}", args.join(" ")).trim().to_string())
            }
            _ => None,
        })
        .collect();
    Report {
        package: test
            .named
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        manifest_sha256: digest,
        application: manifest.application.id.clone(),
        version: manifest.application.version.clone(),
        xpack: env!("CARGO_PKG_VERSION").to_string(),
        mode: if test.real { "real".into() } else { "plan".into() },
        scope: match test.scope {
            InstallScope::User => "user".into(),
            InstallScope::Machine => "machine".into(),
        },
        passed,
        scenarios,
        points,
        left_behind,
        programs: programs.into_iter().collect(),
    }
}

/// Why a declared hook point did not run.
fn not_run_because(point: HookPoint, update_started: bool, previous: bool) -> String {
    match point.operation {
        Operation::Update | Operation::Rollback if !previous => {
            "no --previous release was given".into()
        }
        Operation::Rollback if !update_started => {
            "this package has no update hook that ran, so its rollback hooks never run on a \
             user's machine either"
                .into()
        }
        _ => "its scenario did not reach it".into(),
    }
}

/// What the install and update hooks wrote outside the installations, and
/// nothing removed: by the record, and in real mode also as found on disk.
fn left_behind(lines: &[PlanLine], roots: &[&InstallPaths], real: bool) -> Vec<PathBuf> {
    let inside = |path: &Path| {
        roots.iter().any(|paths| {
            let root = paths.root().canonicalize().unwrap_or_else(|_| paths.root().to_path_buf());
            path.starts_with(&root) || path.starts_with(paths.root())
        })
    };
    let mut written: BTreeSet<PathBuf> = BTreeSet::new();
    let mut removed: Vec<PathBuf> = Vec::new();
    for line in lines {
        let writes = matches!(line.point.operation, Operation::Install | Operation::Update);
        match &line.action {
            PlanAction::Write { path } | PlanAction::MakeDir { path } if writes => {
                written.insert(path.clone());
            }
            PlanAction::Copy { to, .. } if writes => {
                written.insert(to.clone());
            }
            PlanAction::Move { from, to } => {
                removed.push(from.clone());
                if writes {
                    written.insert(to.clone());
                }
            }
            PlanAction::Remove { path } => removed.push(path.clone()),
            _ => {}
        }
    }
    written
        .into_iter()
        .filter(|path| !inside(path))
        .filter(|path| !removed.iter().any(|gone| path.starts_with(gone)))
        .filter(|path| !real || path.exists())
        .collect()
}

/// Watches each hook point start, and checks the installation is as its
/// moment promises.
struct Observer {
    version: Version,
    watching: Mutex<Option<(InstallPaths, Option<Version>)>>,
    held: Mutex<BTreeMap<HookPoint, bool>>,
}

impl Observer {
    fn new(version: Version) -> Self {
        Self { version, watching: Mutex::new(None), held: Mutex::new(BTreeMap::new()) }
    }

    /// From now on, `paths`, where `previous` is the release this one is
    /// applied over, if any.
    fn watch(&self, paths: &InstallPaths, previous: Option<Version>) {
        *self.watching.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((paths.clone(), previous));
    }

    fn held(&self) -> BTreeMap<HookPoint, bool> {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Whether `paths` is as `point`'s moment promises.
    fn moment_holds(
        &self,
        paths: &InstallPaths,
        previous: Option<&Version>,
        point: HookPoint,
    ) -> bool {
        let active = xpack_core::store::load::<xpack_core::InstallState>(&paths.state_file())
            .ok()
            .and_then(|state| state.value.current_version);
        let mine = paths.has_version_files(&self.version);
        let this = Some(&self.version);
        match (point.operation, point.moment) {
            (Operation::Install, Moment::Before) => !mine && active.is_none(),
            (Operation::Install, Moment::AfterFiles) => mine && active.is_none(),
            (Operation::Update, Moment::Before) | (Operation::Rollback, Moment::After) => {
                active.as_ref() == previous
            }
            (Operation::Install | Operation::Update | Operation::Rollback, _) => {
                active.as_ref() == this
            }
            (Operation::Uninstall, Moment::Before) => mine,
            (Operation::Uninstall, _) => !paths.versions_dir().exists(),
        }
    }
}

impl ProgressReporter for Observer {
    fn report(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::RunningHooks { point } => {
                let watching =
                    self.watching.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some((paths, previous)) = watching.as_ref() else { return };
                // The previous release's own hooks are its business.
                if point.operation == Operation::Install && previous.is_some() {
                    let applying = paths.has_version_files(&self.version);
                    if !applying {
                        return;
                    }
                }
                let holds = self.moment_holds(paths, previous.as_ref(), *point);
                self.held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(*point)
                    .and_modify(|held| *held &= holds)
                    .or_insert(holds);
            }
            ProgressEvent::HookOutput { point, line } => {
                xpack_core::errln!("[{point}] {line}");
            }
            _ => {}
        }
    }
}
