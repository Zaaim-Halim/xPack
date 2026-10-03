//! `xpack-hook`: reads a request on standard input, runs its hook, and exits
//! with how the hook ended. See [`xpack_hook::exit`].
//!
//! `xpack-hook --check <script>...` parses each script and runs none: `0`
//! when every one parses, `1` naming each that does not.

use std::io::Read;
use std::process::ExitCode;

use xpack_hook::{Outcome, Request, check, exit, run};

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|first| first == "--check") {
        return check_scripts(&arguments[1..]);
    }
    if !arguments.is_empty() {
        xpack_core::errln!(
            "xpack-hook: takes its request on standard input, or --check <script>..."
        );
        return ExitCode::from(exit::BAD_REQUEST);
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        xpack_core::errln!("xpack-hook: could not read the request: {error}");
        return ExitCode::from(exit::BAD_REQUEST);
    }
    let request: Request = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            xpack_core::errln!("xpack-hook: the request cannot be read: {error}");
            return ExitCode::from(exit::BAD_REQUEST);
        }
    };
    if let Err(problem) = request.check() {
        xpack_core::errln!("xpack-hook: {problem}");
        return ExitCode::from(exit::BAD_REQUEST);
    }
    let source = match std::fs::read_to_string(&request.script) {
        Ok(source) => source,
        Err(error) => {
            xpack_core::errln!("xpack-hook: {}: {error}", request.script.display());
            return ExitCode::from(exit::FAILED);
        }
    };
    match run(&request, &source) {
        Outcome::Succeeded => ExitCode::from(exit::SUCCEEDED),
        Outcome::Failed(reason) => {
            xpack_core::errln!("xpack-hook: {}: {reason}", request.point);
            ExitCode::from(exit::FAILED)
        }
        Outcome::TimedOut => {
            xpack_core::errln!("xpack-hook: {}: still running at its deadline", request.point);
            ExitCode::from(exit::TIMED_OUT)
        }
    }
}

fn check_scripts(scripts: &[std::ffi::OsString]) -> ExitCode {
    if scripts.is_empty() {
        xpack_core::errln!("xpack-hook: --check names no script");
        return ExitCode::from(exit::BAD_REQUEST);
    }
    let mut all_parse = true;
    for script in scripts {
        let path = std::path::Path::new(script);
        let name = path.file_name().map_or_else(|| "hook.js".into(), |n| n.to_string_lossy());
        let parsed =
            std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| check(&name, &s));
        if let Err(problem) = parsed {
            xpack_core::errln!("xpack-hook: {}: {problem}", path.display());
            all_parse = false;
        }
    }
    ExitCode::from(if all_parse { exit::SUCCEEDED } else { exit::FAILED })
}
