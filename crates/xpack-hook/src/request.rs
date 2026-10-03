//! What a hook's programs are given of this program's environment.

use std::collections::BTreeMap;

pub use xpack_core::hooks::Request;

/// The variables a program a hook runs needs from this process's own
/// environment, and nothing else of it.
///
/// Built from a list of what programs need rather than from everything minus
/// what is known to be secret, so a password the installer was given, under
/// whatever name, never reaches a hook's program.
const PASSED_THROUGH: [&str; 17] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USERPROFILE",
    "USERNAME",
    "TMP",
    "TEMP",
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATHEXT",
];

/// The environment a hook's programs run with: what they need from this one,
/// the application's launch environment, and nothing of xPack's own.
pub(crate) fn program_environment(request: &Request) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = std::env::vars()
        .filter(|(name, _)| PASSED_THROUGH.iter().any(|allowed| allowed.eq_ignore_ascii_case(name)))
        .collect();
    for (name, value) in &request.environment {
        if !steers_programs(name) {
            environment.insert(name.clone(), value.clone());
        }
    }
    environment
}

/// A variable a hook's programs never get from a hook or a package: one that
/// steers xPack or carries one of its secrets, or one that changes which code
/// a program runs rather than what it does.
///
/// `PATH` among them: the program itself is found before it starts, but
/// Windows also looks along `PATH` for the libraries a program loads, and a
/// program looks along it for whatever it starts in turn.
pub(crate) fn steers_programs(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name == "PATH" || ["XPACK_", "LD_", "DYLD_"].iter().any(|prefix| name.starts_with(prefix))
}
