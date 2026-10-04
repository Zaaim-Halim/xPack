//! The release gate: a package that declares hooks ships only with a
//! passing `xpack hooks test` report for that exact package beside it.
//!
//! Used by every command that ships a version: `xpack installer`,
//! `xpack index` and `xpack delta`. There is no override. A publisher who
//! wants to ship without testing a hook has one way to do it: remove the
//! hook.
//!
//! What it is, and is not: protection against a publisher's mistakes, a
//! hook never run, run against another build, or changed without anyone
//! noticing. Not against a publisher who means harm and holds the signing
//! key: nothing in a packaging tool can be.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use xpack_core::hooks::HookPoint;
use xpack_core::{Error, InstallScope, Manifest, Result};
use xpack_update::HookSummary;

use super::hooks_test::{Report, report_path};

/// A package about to ship, as the gate needs to see it.
pub(crate) struct Shipping<'a> {
    /// The package as it was named, beside which its report sits.
    pub(crate) package: &'a Path,
    /// The SHA-256 of its signed manifest, from
    /// [`package_identity`](super::hooks_test::package_identity): what its
    /// report must be bound to.
    pub(crate) manifest_sha256: xpack_core::Sha256Digest,
    pub(crate) manifest: &'a Manifest,
    /// Whether installations will reach it from an earlier release: the
    /// index being written already offers one, or it ships as a delta.
    pub(crate) earlier_release: bool,
    /// Who it can be installed for: a passing report is needed for each, as
    /// each has permissions of its own.
    pub(crate) scopes: &'a [InstallScope],
}

/// Refuses to ship a package whose hooks have not passed `xpack hooks test`
/// for that exact package, in each scope it can be installed for. A package
/// without hooks always passes.
pub(crate) fn ensure_tested(shipping: &Shipping<'_>) -> Result<()> {
    if shipping.manifest.hooks.is_empty() {
        return Ok(());
    }
    for scope in shipping.scopes {
        ensure_tested_for(shipping, *scope)?;
    }
    Ok(())
}

/// [`ensure_tested`], for installations of one `scope`.
fn ensure_tested_for(shipping: &Shipping<'_>, scope: InstallScope) -> Result<()> {
    let manifest = shipping.manifest;
    let what = format!("{} {}", manifest.application.name, manifest.application.version);
    let package = shipping.package.display();
    let (everyone, whom) = match scope {
        InstallScope::User => ("", ""),
        InstallScope::Machine => (" --all-users", ", for an installation for everyone,"),
    };
    let run = format!("xpack hooks test {package} --previous <the last release>{everyone}");
    let refuse = |why: String| {
        Error::invalid(
            "hooks",
            format!(
                "{what} declares hooks, and they have not passed `xpack hooks test`{whom} for \
                 this package ({why}). Run:\n  {run}"
            ),
        )
    };

    let report_file = report_path(shipping.package, scope);
    let bytes = match std::fs::read(&report_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(refuse("no report beside it".into()));
        }
        Err(error) => return Err(Error::io(&report_file, error)),
    };
    let report: Report = serde_json::from_slice(&bytes)
        .map_err(|_| refuse(format!("{} cannot be read", report_file.display())))?;

    let tested = if report.scope == "machine" { InstallScope::Machine } else { InstallScope::User };
    if tested != scope {
        return Err(refuse(format!("{} tested the other scope", report_file.display())));
    }
    if report.manifest_sha256 != shipping.manifest_sha256 {
        return Err(refuse("the report beside it is for another build".into()));
    }
    if !report.passed {
        let failed: Vec<&str> = report
            .points
            .iter()
            .filter(|point| point.result != "succeeded" && point.result != "not run")
            .map(|point| point.point.as_str())
            .collect();
        let named =
            if failed.is_empty() { String::new() } else { format!(": {}", failed.join(", ")) };
        return Err(refuse(format!("the report beside it failed{named}")));
    }
    let has_update_hooks = !manifest.hooks.update.is_empty();
    let update_ran = report.scenarios.iter().any(|s| s.name == "update" && s.ran);
    if shipping.earlier_release && has_update_hooks && !update_ran {
        return Err(refuse(
            "it has update hooks, installations reach it from an earlier release, and the report \
             ran no update scenario"
                .into(),
        ));
    }
    if manifest.update.mandatory && report.mode != "real" {
        return Err(Error::invalid(
            "hooks",
            format!(
                "{what} is mandatory, so installations cannot skip it, and its hooks were only \
                 tested in plan mode; they must have had a real run. On a disposable machine of \
                 its platform (a CI runner is one), run:\n  {run} --real"
            ),
        ));
    }
    Ok(())
}

/// A release's hooks as the index records them, for the next release to be
/// compared with. None when it has none.
pub(crate) fn summary(manifest: &Manifest) -> Option<HookSummary> {
    if manifest.hooks.is_empty() {
        return None;
    }
    let scripts = manifest
        .hooks
        .all()
        .filter_map(|(_, hook)| {
            manifest
                .payload
                .files
                .iter()
                .find(|file| file.path == hook.script)
                .map(|file| (hook.script.clone(), file.sha256))
        })
        .collect();
    Some(HookSummary { declared: manifest.hooks.clone(), scripts })
}

/// How a release's hooks differ from the one before: every difference, and
/// apart from them the permissions it gains, which are always shown.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub(crate) differences: Vec<String>,
    pub(crate) new_permissions: Vec<String>,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.differences.is_empty() && self.new_permissions.is_empty()
    }
}

/// The differences between `before`'s hooks and `after`'s.
pub(crate) fn changes(before: Option<&HookSummary>, after: Option<&HookSummary>) -> Changes {
    let empty =
        HookSummary { declared: xpack_core::hooks::Hooks::default(), scripts: BTreeMap::new() };
    let before = before.unwrap_or(&empty);
    let after = after.unwrap_or(&empty);
    let mut differences = Vec::new();

    for (script, digest) in &after.scripts {
        match before.scripts.get(script) {
            None => differences.push(format!("new script {script}")),
            Some(old) if old != digest => differences.push(format!("{script} changed")),
            Some(_) => {}
        }
    }
    for script in before.scripts.keys() {
        if !after.scripts.contains_key(script) {
            differences.push(format!("{script} is no longer run"));
        }
    }
    let points = |hooks: &xpack_core::hooks::Hooks| -> BTreeMap<HookPoint, Vec<String>> {
        let mut points: BTreeMap<HookPoint, Vec<String>> = BTreeMap::new();
        for (point, hook) in hooks.all() {
            let timeout = hook.timeout_seconds.map(|t| format!(" ({t} s)")).unwrap_or_default();
            points.entry(point).or_default().push(format!("{}{timeout}", hook.script));
        }
        points
    };
    let (old_points, new_points) = (points(&before.declared), points(&after.declared));
    let every: BTreeSet<&HookPoint> = old_points.keys().chain(new_points.keys()).collect();
    for point in every {
        let (old, new) = (old_points.get(point), new_points.get(point));
        if old != new {
            let list = |scripts: Option<&Vec<String>>| {
                scripts.map_or_else(|| "nothing".to_string(), |scripts| scripts.join(", "))
            };
            differences.push(format!("{point}: {} → {}", list(old), list(new)));
        }
    }

    let mut new_permissions = Vec::new();
    for (scope, old, new) in [
        ("user", &before.declared.permissions.user, &after.declared.permissions.user),
        ("machine", &before.declared.permissions.machine, &after.declared.permissions.machine),
    ] {
        for program in new.exec.iter().filter(|p| !old.exec.contains(p)) {
            new_permissions.push(format!("{scope}: may run {program}"));
        }
        for place in new.write.iter().filter(|p| !old.write.contains(p)) {
            new_permissions.push(format!("{scope}: may write {place}"));
        }
        for program in old.exec.iter().filter(|p| !new.exec.contains(p)) {
            differences.push(format!("{scope}: may no longer run {program}"));
        }
        for place in old.write.iter().filter(|p| !new.write.contains(p)) {
            differences.push(format!("{scope}: may no longer write {place}"));
        }
    }
    Changes { differences, new_permissions }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_of(hooks: &str, scripts: &[(&str, u8)]) -> HookSummary {
        HookSummary {
            declared: serde_json::from_str(hooks).unwrap(),
            scripts: scripts
                .iter()
                .map(|(path, byte)| {
                    ((*path).to_string(), xpack_core::Sha256Digest::from_bytes([*byte; 32]))
                })
                .collect(),
        }
    }

    #[test]
    fn the_same_hooks_are_no_change() {
        let a = summary_of(r#"{"install":"a.js"}"#, &[("a.js", 1)]);
        assert!(changes(Some(&a), Some(&a.clone())).is_empty());
        assert!(changes(None, None).is_empty());
    }

    #[test]
    fn a_script_changed_added_or_dropped_is_a_change() {
        let before = summary_of(r#"{"install":["a.js","b.js"]}"#, &[("a.js", 1), ("b.js", 2)]);
        let after = summary_of(r#"{"install":["a.js","c.js"]}"#, &[("a.js", 9), ("c.js", 3)]);
        let found = changes(Some(&before), Some(&after)).differences;
        assert!(found.contains(&"a.js changed".to_string()), "{found:?}");
        assert!(found.contains(&"new script c.js".to_string()), "{found:?}");
        assert!(found.contains(&"b.js is no longer run".to_string()), "{found:?}");
        assert!(found.iter().any(|d| d.starts_with("install.after:")), "{found:?}");
    }

    #[test]
    fn a_hook_moved_or_given_a_new_timeout_is_a_change() {
        let before = summary_of(r#"{"update":"a.js"}"#, &[("a.js", 1)]);
        let moved = summary_of(r#"{"update":{"when":"before","script":"a.js"}}"#, &[("a.js", 1)]);
        assert_ne!(changes(Some(&before), Some(&moved)).differences, Vec::<String>::new());
        let slower =
            summary_of(r#"{"update":{"script":"a.js","timeoutSeconds":900}}"#, &[("a.js", 1)]);
        assert_ne!(changes(Some(&before), Some(&slower)).differences, Vec::<String>::new());
    }

    #[test]
    fn new_permissions_are_named_apart_from_other_changes() {
        let before = summary_of(r#"{"install":"a.js"}"#, &[("a.js", 1)]);
        let after = summary_of(
            r#"{"install":"a.js","permissions":{"user":{"exec":["systemctl"],"write":["{home}/.config"]}}}"#,
            &[("a.js", 1)],
        );
        let found = changes(Some(&before), Some(&after));
        assert_eq!(
            found.new_permissions,
            ["user: may run systemctl", "user: may write {home}/.config"]
        );
        assert!(found.differences.is_empty(), "{found:?}");
    }

    /// A permission both releases have is neither new nor dropped; one only
    /// the earlier had is named as dropped.
    #[test]
    fn a_permission_kept_is_no_change_and_one_dropped_is_named() {
        let both =
            r#"{"install":"a.js","permissions":{"user":{"exec":["git"],"write":["{home}/.a"]}}}"#;
        let before = summary_of(both, &[("a.js", 1)]);
        assert!(changes(Some(&before), Some(&before.clone())).is_empty());
        let after = summary_of(r#"{"install":"a.js"}"#, &[("a.js", 1)]);
        let found = changes(Some(&before), Some(&after));
        assert_eq!(
            found.differences,
            ["user: may no longer run git", "user: may no longer write {home}/.a"]
        );
        assert!(found.new_permissions.is_empty(), "{found:?}");
    }

    #[test]
    fn hooks_added_to_a_release_that_had_none_are_a_change() {
        let after = summary_of(r#"{"install":"a.js"}"#, &[("a.js", 1)]);
        assert!(!changes(None, Some(&after)).is_empty());
    }
}
