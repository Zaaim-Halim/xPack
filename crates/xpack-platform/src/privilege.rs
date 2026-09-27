//! Administrator rights: whether this process has them, how to ask for them,
//! and what a process that has them must not leave behind.
//!
//! An installation for every user of the machine is written by a process with
//! administrator rights, started on behalf of a user who may not have them.
//! Everything about that process is a place for that user to reach further
//! than they could on their own, so this module is small and says exactly
//! what it does.

use std::ffi::OsString;
use std::path::Path;

use xpack_core::{Error, Result};

/// Whether this process runs with administrator rights: as root on macOS and
/// Linux, elevated on Windows.
pub fn is_elevated() -> bool {
    imp::is_elevated()
}

/// Makes every file this process creates from here on unwritable by anyone
/// but its owner, whatever the umask it was started with.
///
/// For an elevated installer: a file it writes belongs to root, and a
/// permissive umask inherited from the user's shell would otherwise leave it
/// writable by everyone, which is every user able to change what every user
/// runs. Nothing to do on Windows, where a new file takes the access list of
/// the directory it is created in.
pub fn restrict_new_files() {
    imp::restrict_new_files();
}

/// What asking for administrator rights to run a program came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevated {
    /// It ran, and exited with this code.
    Exited(i32),
    /// The person asked declined, or did not have the rights.
    Declined,
}

/// Runs `program` with `arguments` with administrator rights, asking for them
/// the way this platform does, and waits for it.
///
/// - macOS: the system's administrator prompt, through `osascript`.
/// - Linux: `pkexec`, which asks through the desktop's own prompt.
/// - Windows: the UAC prompt.
///
/// The program is started with nothing of this process's environment it
/// could be steered by: the elevated side must never trust it anyway, and
/// every platform's route drops most of it.
pub fn run_elevated(program: &Path, arguments: &[OsString]) -> Result<Elevated> {
    imp::run_elevated(program, arguments)
}

/// Quotes `value` for `/bin/sh`: single quotes, with the usual escape for an
/// embedded one.
#[cfg(any(target_os = "macos", test))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Quotes `value` as a string literal in `AppleScript`.
#[cfg(any(target_os = "macos", test))]
fn applescript_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The line an elevated macOS run executes: the program and its arguments,
/// quoted for the shell, followed by a marker carrying its exit code, because
/// `do shell script` reports a failure without the code.
#[cfg(any(target_os = "macos", test))]
fn macos_command(program: &Path, arguments: &[OsString]) -> String {
    let mut line = shell_quote(&program.to_string_lossy());
    for argument in arguments {
        line.push(' ');
        line.push_str(&shell_quote(&argument.to_string_lossy()));
    }
    format!("{line}; echo {EXIT_MARKER}$?")
}

/// Precedes the exit code in what an elevated macOS run prints.
#[cfg(any(target_os = "macos", test))]
const EXIT_MARKER: &str = "xpack-exit=";

#[cfg(unix)]
mod imp {
    use super::{Elevated, Error, OsString, Path, Result};

    pub(super) fn is_elevated() -> bool {
        rustix::process::geteuid().is_root()
    }

    pub(super) fn restrict_new_files() {
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
    }

    #[cfg(target_os = "macos")]
    pub(super) fn run_elevated(program: &Path, arguments: &[OsString]) -> Result<Elevated> {
        let script = format!(
            "do shell script {} with administrator privileges",
            super::applescript_quote(&super::macos_command(program, arguments))
        );
        let output = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| Error::Launch(format!("asking for administrator rights: {e}")))?;
        let printed = String::from_utf8_lossy(&output.stdout);
        if let Some(code) = printed
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix(super::EXIT_MARKER))
            .and_then(|code| code.parse().ok())
        {
            return Ok(Elevated::Exited(code));
        }
        // No marker: the prompt was cancelled (error -128) or refused.
        Ok(Elevated::Declined)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn run_elevated(program: &Path, arguments: &[OsString]) -> Result<Elevated> {
        let status = std::process::Command::new("pkexec")
            .arg(program)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .status()
            .map_err(|e| {
                Error::Launch(format!("asking for administrator rights with pkexec: {e}"))
            })?;
        // 126: the person did not give the rights; 127: they could not.
        Ok(match status.code() {
            Some(126 | 127) | None => Elevated::Declined,
            Some(code) => Elevated::Exited(code),
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    use super::{Elevated, Error, OsString, Path, Result};

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Quotes one argument the way the C runtime splits a command line.
    fn quote(argument: &std::ffi::OsStr) -> String {
        let argument = argument.to_string_lossy();
        if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
            return argument.into_owned();
        }
        let mut quoted = String::from('"');
        let mut backslashes = 0;
        for c in argument.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                other => {
                    quoted.push_str(&"\\".repeat(backslashes));
                    quoted.push(other);
                    backslashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(backslashes * 2));
        quoted.push('"');
        quoted
    }

    pub(super) fn is_elevated() -> bool {
        // SAFETY: the pseudo-handle `GetCurrentProcess` returns needs no
        // closing; `token` is a live local the call fills and is closed
        // below; `elevation` is a live local of exactly the size passed,
        // which is what `TokenElevation` writes.
        #[allow(unsafe_code)]
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut written = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                (&raw mut elevation).cast(),
                u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
                &raw mut written,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    pub(super) fn restrict_new_files() {}

    pub(super) fn run_elevated(program: &Path, arguments: &[OsString]) -> Result<Elevated> {
        let verb = wide(std::ffi::OsStr::new("runas"));
        let file = wide(program.as_os_str());
        let parameters: Vec<String> = arguments.iter().map(|a| quote(a)).collect();
        let parameters = wide(std::ffi::OsStr::new(&parameters.join(" ")));

        // SAFETY: every pointer in `info` points into a live, NUL-terminated
        // buffer owned by this frame and outlives the call; the struct is
        // zeroed and then given its own size, as the call requires. The
        // process handle it returns is waited on, read and closed here, and
        // not used after.
        #[allow(unsafe_code)]
        unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0);
            info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = file.as_ptr();
            info.lpParameters = parameters.as_ptr();
            info.nShow = 1;
            if ShellExecuteExW(&raw mut info) == 0 {
                let error = GetLastError();
                if error == ERROR_CANCELLED {
                    return Ok(Elevated::Declined);
                }
                return Err(Error::Launch(format!(
                    "asking for administrator rights failed (error {error})"
                )));
            }
            if info.hProcess.is_null() {
                return Err(Error::Launch("the elevated program gave no process".into()));
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(info.hProcess, &raw mut code);
            CloseHandle(info.hProcess);
            Ok(Elevated::Exited(i32::from_ne_bytes(code.to_ne_bytes())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_macos_command_keeps_every_argument_whole_and_reports_its_code() {
        let line = macos_command(
            Path::new("/Users/Ada Lovelace/Downloads/Setup"),
            &["--all-users".into(), "it's; rm -rf /".into()],
        );
        assert_eq!(
            line,
            r"'/Users/Ada Lovelace/Downloads/Setup' '--all-users' 'it'\''s; rm -rf /'; echo xpack-exit=$?"
        );
    }

    #[test]
    fn an_applescript_string_cannot_be_closed_early() {
        assert_eq!(applescript_quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    #[test]
    fn a_test_run_is_not_elevated_unless_it_really_is() {
        // Not an assertion either way: the suite may run as root in CI. It
        // must answer without failing.
        let _ = is_elevated();
    }
}
