//! Stands in for an installed application, on every platform.
//!
//! Prints what it was given so a test can see the arguments and environment
//! arrived, then exits with the status the manifest asked for. That status is
//! the point: probation, rollback and "the application's own exit code passes
//! through" all turn on it.

/// The exit status to finish with, chosen by the manifest that launched this.
const EXIT_ENV: &str = "XPACK_TEST_PAYLOAD_EXIT";

/// Which version this copy belongs to, so a test can tell them apart.
const VERSION_ENV: &str = "XPACK_TEST_PAYLOAD_VERSION";

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let version = std::env::var(VERSION_ENV).unwrap_or_default();
    println!("running {version} args={}", arguments.join(" "));

    // Reported so a test can prove the launcher passed them through.
    for key in ["XPACK_HEALTH_FILE", "XPACK_APPLICATION_DIR"] {
        println!("{key}={}", std::env::var(key).unwrap_or_default());
    }
    // And where it was started, which the manifest decides.
    let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    println!("cwd={cwd}");

    // Writing the health file is what an application does to say it started,
    // and some tests require the report rather than inferring one.
    if let (Ok(health), Ok(_)) =
        (std::env::var("XPACK_HEALTH_FILE"), std::env::var("XPACK_TEST_REPORT_HEALTH"))
    {
        let _ = std::fs::write(&health, "ok\n");
    }

    let code: u8 = std::env::var(EXIT_ENV).ok().and_then(|value| value.parse().ok()).unwrap_or(0);
    std::process::ExitCode::from(code)
}
