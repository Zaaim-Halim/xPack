//! `xpack-hook`: reads a request on standard input, runs its hook, and exits
//! with how the hook ended. See [`xpack_hook::exit`].

use std::io::Read;
use std::process::ExitCode;

use xpack_hook::{Outcome, Request, exit, run};

fn main() -> ExitCode {
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
