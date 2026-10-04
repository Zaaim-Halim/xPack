//! `xpack-hook`: reads a request on standard input, runs its hook, and exits
//! with how the hook ended. See [`xpack_hook::exit`].
//!
//! `xpack-hook --check <script>...` parses each script and runs none: `0`
//! when every one parses, `1` naming each that does not.

use std::io::{BufRead, Read};
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
    // The request is its first line. A runner that keeps the input open
    // after it says so in the request; one that closes it has sent all.
    let mut input = String::new();
    if let Err(error) = std::io::stdin().lock().read_line(&mut input) {
        xpack_core::errln!("xpack-hook: could not read the request: {error}");
        return ExitCode::from(exit::BAD_REQUEST);
    }
    let mut rest = String::new();
    let request: Request = match serde_json::from_str(&input) {
        Ok(request) => request,
        // Perhaps written over several lines by a runner that then closed
        // the input: the whole of it is the request.
        Err(_) if std::io::stdin().lock().read_to_string(&mut rest).is_ok() && !rest.is_empty() => {
            match serde_json::from_str(&(input + &rest)) {
                Ok(request) => request,
                Err(error) => {
                    xpack_core::errln!("xpack-hook: the request cannot be read: {error}");
                    return ExitCode::from(exit::BAD_REQUEST);
                }
            }
        }
        Err(error) => {
            xpack_core::errln!("xpack-hook: the request cannot be read: {error}");
            return ExitCode::from(exit::BAD_REQUEST);
        }
    };
    if request.stop_with_runner {
        stop_when_the_runner_goes();
    }
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

/// Watches the input the runner holds open: it closes when the runner ends,
/// however it ends. One that was killed would never stop this hook at its
/// deadline, so it stops now, with everything it started.
fn stop_when_the_runner_goes() {
    std::thread::spawn(|| {
        let mut sink = [0_u8; 256];
        loop {
            match std::io::stdin().read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        xpack_platform::tree::stop_own_tree();
        std::process::exit(i32::from(exit::FAILED));
    });
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
