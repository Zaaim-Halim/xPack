//! A package's hooks, checked before anything ships: `xpack hooks check`,
//! and the checks `xpack pack` runs on every package it builds.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args as ClapArgs, Subcommand};
use xpack_core::hooks::{Moment, Operation};
use xpack_core::manifest::{PayloadFile, PayloadSpec};
use xpack_core::{Error, Manifest, Platform, Result};

use crate::config::ProjectConfig;

/// Arguments for `xpack hooks`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: HooksCommand,
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Check a project's hooks as `xpack pack` does, without building.
    Check(CheckArgs),
}

/// Arguments for `xpack hooks check`.
#[derive(ClapArgs)]
struct CheckArgs {
    /// Directory whose contents become the payload.
    #[arg(value_name = "PAYLOAD_DIR")]
    payload: PathBuf,

    /// Project configuration.
    #[arg(long, value_name = "FILE", default_value = "xpack.json")]
    config: PathBuf,
}

/// Runs `xpack hooks`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    match &args.command {
        HooksCommand::Check(check) => {
            let config = ProjectConfig::load(&check.config)?;
            let manifest = config.to_manifest(Platform::host()?);
            let warnings = check_hooks(&manifest, &check.payload)?;
            print_warnings(&warnings);
            let scripts = scripts_of(&manifest).len();
            crate::output::field("hooks", format!("ok, {scripts} script(s)"));
            super::success()
        }
    }
}

/// Prints what [`check_hooks`] warned about, on standard error.
pub(crate) fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        xpack_core::errln!("warning: {warning}");
    }
}

/// Checks the hooks `manifest` declares against the payload in `payload`:
/// everything that can be known before the package exists. Refuses, naming
/// the hook and the rule, anything that could never run; returns what is
/// worth a warning.
///
/// The rules: the declaration itself (moments, timeouts, scripts listed
/// twice, permissions each scope may hold); each script is a regular file
/// of the payload, valid UTF-8, a `.js` file of at most 1 MiB, with no
/// `require` or dynamic `import`; and each loads in `xpack-hook`, imports
/// nothing and exports a function `main`. `xpack-hook` must be beside this
/// program for that last part: a package whose scripts were never loaded is
/// not built.
pub(crate) fn check_hooks(manifest: &Manifest, payload: &Path) -> Result<Vec<String>> {
    if manifest.hooks.is_empty() {
        return Ok(Vec::new());
    }
    let scripts = scripts_of(manifest);

    // The declaration, against what is really in the payload.
    let mut spec = PayloadSpec::default();
    for script in &scripts {
        let file = file_of(payload, script);
        if let Ok(metadata) = std::fs::symlink_metadata(&file) {
            if !metadata.file_type().is_file() {
                return Err(refuse(script, "is not a regular file"));
            }
            spec.files.push(PayloadFile {
                path: script.clone(),
                size: metadata.len(),
                sha256: xpack_core::Sha256Digest::from_bytes([0; xpack_core::SHA256_LEN]),
                mode: None,
            });
        }
    }
    manifest.hooks.validate(&spec)?;

    for script in &scripts {
        let bytes = std::fs::read(file_of(payload, script)).map_err(|e| Error::io(script, e))?;
        let text = String::from_utf8(bytes).map_err(|_| refuse(script, "is not valid UTF-8"))?;
        if let Some(what) = reaches_for_another_file(&text) {
            return Err(refuse(script, &format!("uses {what}; a hook is one file")));
        }
    }

    let engine = super::sibling_binary_required("xpack-hook").map_err(|_| {
        Error::invalid(
            "hooks",
            "xpack-hook was not found beside this executable, so the hook scripts cannot be \
             checked; a package whose scripts were never loaded is not built",
        )
    })?;
    xpack_install::hooks::check_scripts(&engine, manifest, payload)?;

    Ok(warnings(manifest))
}

/// What is worth saying about hooks that will run, without refusing them.
fn warnings(manifest: &Manifest) -> Vec<String> {
    let hooks = &manifest.hooks;
    let mut warnings = Vec::new();
    if !hooks.install.is_empty() && hooks.uninstall.is_empty() {
        warnings.push(
            "this package has install hooks and no uninstall hook: what they install, nothing \
             removes"
                .into(),
        );
    }
    if !hooks.update.is_empty() && hooks.rollback.is_empty() {
        warnings.push(
            "this package has update hooks and no rollback hook: what they change, nothing \
             undoes when the version is rolled back"
                .into(),
        );
    }
    let at = |moment| xpack_core::hooks::HookPoint { operation: Operation::Update, moment };
    let after = hooks.at(at(Moment::After)).next().is_some();
    let confirmed = hooks.at(at(Moment::Confirmed)).next().is_some();
    if manifest.update.mandatory && after && !confirmed {
        warnings.push(
            "this release is mandatory and does its update work in update.after, which runs \
             before the version has proved it starts; work that cannot be undone belongs in \
             update.confirmed"
                .into(),
        );
    }
    warnings
}

/// Every script the hooks name, once each.
fn scripts_of(manifest: &Manifest) -> Vec<String> {
    let mut scripts: Vec<String> =
        manifest.hooks.all().map(|(_, hook)| hook.script.clone()).collect();
    scripts.sort();
    scripts.dedup();
    scripts
}

fn file_of(payload: &Path, script: &str) -> PathBuf {
    script.split('/').fold(payload.to_path_buf(), |path, part| path.join(part))
}

fn refuse(script: &str, why: &str) -> Error {
    Error::invalid("hooks", format!("{script} {why}"))
}

/// `require(` or a dynamic `import(` in `source`, outside comments and
/// string literals: both reach for another file, and both would fail only
/// when the hook runs, on a user's machine. A static `import` is caught when
/// the script is loaded.
///
/// A scan, not a parser: a regular expression literal holding one of them
/// would be taken for code. That refuses a script that would have run, never
/// passes one that would not.
fn reaches_for_another_file(source: &str) -> Option<&'static str> {
    let code = without_comments_and_strings(source);
    let words = |word: &str| {
        code.match_indices(word).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            let after = code[at + word.len()..].trim_start().chars().next();
            let starts_word =
                before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '.'));
            starts_word && after == Some('(')
        })
    };
    if words("require") {
        Some("require")
    } else if words("import") {
        Some("a dynamic import")
    } else {
        None
    }
}

/// `source` with its comments and the contents of its string and template
/// literals blanked out, so a word in them is never taken for code.
fn without_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                out.push(' ');
            }
            '\'' | '"' | '`' => {
                let quote = c;
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        chars.next();
                    } else if c == quote {
                        break;
                    }
                }
                out.push_str("\"\"");
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_and_dynamic_import_are_found_in_code_and_nowhere_else() {
        for code in [
            "const fs = require('fs');",
            "const m = await import('./other.js');",
            "x = require ('a')",
            "f(import(\"x\"))",
        ] {
            assert!(reaches_for_another_file(code).is_some(), "{code}");
        }
        for not_code in [
            "// require('fs')",
            "/* import('x') */ export function main() {}",
            "ctx.log.info('require(x)');",
            "const s = \"import('y')\";",
            "const t = `require(${a})`;",
            "ctx.required(1); myrequire(2); obj.import(3);",
            "export function main(ctx) { ctx.log.info('ok'); }",
        ] {
            assert_eq!(reaches_for_another_file(not_code), None, "{not_code}");
        }
    }
}
