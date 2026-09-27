//! Asking an application to close, from a process that has no console.
//!
//! "Restart now" asks the application to close from the windowed launcher,
//! which has no console. On Windows the request is `taskkill`, a console
//! program, and Windows gives a console program a new console window when its
//! parent has none to share: a black window flashing up over the application
//! the user just agreed to restart.
//!
//! A test process has a console, so it cannot see this by calling
//! `request_close` itself. This file is its own harness instead, and its
//! executable plays three parts, chosen by the name it is copied under:
//!
//! - the test, which copies itself into a directory as the other two and
//!   starts the asker with no console, as a shortcut starts the launcher;
//! - the asker, which asks a process to close;
//! - `taskkill`, found beside the asker before the system's own (Windows looks
//!   in the program's own directory first), which records whether it was
//!   given a console window.
//!
//! A second case starts `taskkill` the plain way and must see a window, so
//! that a detector which could never see one cannot pass the first.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    windows::main();
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// PowerShell, asking Windows for its console window and writing the
    /// handle to the file named in `XPACK_TEST_REPORT`: `0` when it has none.
    /// It shares the console of whoever started it, so the answer is that
    /// process's.
    const REPORT_CONSOLE: &str = "$t = Add-Type -Name Console -Namespace XpackTest -PassThru \
         -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow();'; \
         [System.IO.File]::WriteAllText($env:XPACK_TEST_REPORT, \
         $t::GetConsoleWindow().ToInt64().ToString())";

    /// `DETACHED_PROCESS` from `processthreadsapi.h`: the child starts with no
    /// console at all, which is the windowed launcher's situation.
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    /// Where the fake `taskkill` writes its answer: beside itself.
    const REPORT: &str = "console.txt";

    pub(crate) fn main() {
        let me = std::env::current_exe().expect("this test's own path");
        let role = me.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_owned();
        let pid = std::env::args().nth(1);
        match role.as_str() {
            "taskkill" => report_console(&me),
            "asker" => {
                // The result is not the question here, the window is.
                let _ = xpack_platform::request_close(parse(pid.as_deref()));
            }
            "plain" => {
                let _ = Command::new("taskkill")
                    .arg("/PID")
                    .arg(parse(pid.as_deref()).to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            _ => run_the_tests(&me),
        }
    }

    fn parse(pid: Option<&str>) -> u32 {
        pid.and_then(|pid| pid.parse().ok()).unwrap_or(0)
    }

    /// The fake `taskkill`: reports the console window it was given.
    fn report_console(me: &Path) {
        let report = me.with_file_name(REPORT);
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", REPORT_CONSOLE])
            .env("XPACK_TEST_REPORT", &report)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("PowerShell to start");
        assert!(status.success(), "PowerShell could not report the console: {status}");
    }

    fn run_the_tests(me: &Path) {
        asking_to_close_from_a_process_with_no_console_opens_no_console_window(me);
        println!(
            "test asking_to_close_from_a_process_with_no_console_opens_no_console_window ... ok"
        );
        a_plainly_started_taskkill_gets_a_window_so_the_check_can_see_one(me);
        println!("test a_plainly_started_taskkill_gets_a_window_so_the_check_can_see_one ... ok");
    }

    fn asking_to_close_from_a_process_with_no_console_opens_no_console_window(me: &Path) {
        assert_eq!(
            console_window_seen_by_taskkill(me, "asker"),
            0,
            "asking an application to close opened a console window"
        );
    }

    fn a_plainly_started_taskkill_gets_a_window_so_the_check_can_see_one(me: &Path) {
        assert_ne!(
            console_window_seen_by_taskkill(me, "plain"),
            0,
            "a console program started with no flags got no window: this check cannot see one"
        );
    }

    /// Starts `role` with no console, and returns the console window the fake
    /// `taskkill` it started was given.
    fn console_window_seen_by_taskkill(me: &Path, role: &str) -> i64 {
        let dir = tempfile::tempdir().unwrap();
        let caller = copy_as(me, dir.path(), role);
        copy_as(me, dir.path(), "taskkill");

        let status = Command::new(&caller)
            .arg("1")
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "the {role} failed: {status}");

        let answer = fs::read_to_string(dir.path().join(REPORT))
            .expect("the fake taskkill never answered: the system's own was found first, or it could not write");
        answer.trim().parse().unwrap()
    }

    fn copy_as(me: &Path, dir: &Path, name: &str) -> PathBuf {
        let to = dir.join(format!("{name}.exe"));
        fs::copy(me, &to).unwrap();
        to
    }
}
