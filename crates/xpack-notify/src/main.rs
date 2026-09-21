//! The notifier binary.
//!
//! Spawned with everything it needs on its command line, it opens one dialog
//! and reports what the user said through its exit code. It reads no
//! installation state and writes none: the caller owns all of that.
//!
//! # No console
//!
//! Linked for the Windows subsystem, like the windowed launcher, because the
//! alternative is a black console window opening behind the dialog every time
//! an update is announced. The cost is that a message this binary might print
//! — a command line the parser rejected, above all — goes nowhere on Windows.
//! Its caller is the launcher, not a person at a terminal, and a caller that
//! passes the wrong arguments learns so from the exit code.

// Applied unconditionally: the attribute is accepted everywhere and means
// something only on Windows, so the two builds differ in nothing else.
#![windows_subsystem = "windows"]

use std::process::ExitCode;

use clap::Parser;
use xpack_core::{UpdateSeverity, Version};
use xpack_notify::Prompt;

#[derive(Parser)]
#[command(name = "xpack-notify", about = "Tells a user that an update is installed and waiting")]
struct Args {
    /// Application name, as the user knows it.
    #[arg(long, value_name = "NAME")]
    application: String,

    /// Version waiting to be used.
    #[arg(long, value_name = "VERSION")]
    version: Version,

    /// How urgent the publisher said the release is.
    #[arg(long, value_name = "LEVEL", default_value = "optional", value_parser = parse_severity)]
    severity: UpdateSeverity,

    /// Dialog title. Defaults to wording built from the application and version.
    #[arg(long, value_name = "TEXT")]
    title: Option<String>,

    /// Dialog body. Defaults to wording built from the application and version.
    #[arg(long, value_name = "TEXT")]
    message: Option<String>,

    /// The application's icon, shown in the dialog and on the task bar.
    ///
    /// The installation's own copy, which outlives any one version directory.
    #[arg(long, value_name = "FILE")]
    icon: Option<std::path::PathBuf>,

    /// Offer to restart, because the caller is able to.
    ///
    /// Without it the dialog tells the user the update will be used at the
    /// next start, and offers nothing to accept or decline — which is the
    /// truth when nothing is able to restart the application for them.
    #[arg(long)]
    can_restart: bool,
}

/// Parses a severity, naming the accepted values when it is not one.
fn parse_severity(text: &str) -> Result<UpdateSeverity, String> {
    match text {
        "optional" => Ok(UpdateSeverity::Optional),
        "recommended" => Ok(UpdateSeverity::Recommended),
        "critical" => Ok(UpdateSeverity::Critical),
        other => Err(format!("{other:?} is not one of optional, recommended or critical")),
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    let prompt = Prompt {
        application: args.application,
        version: args.version,
        severity: args.severity,
        title: args.title,
        message: args.message,
        // A path that is not there is the same as none: an icon is decoration,
        // and a dialog that refused to open over a missing one would be worse
        // than a dialog with the system's default.
        icon: args.icon.filter(|path| path.is_file()),
        can_restart: args.can_restart,
    };

    let answer = xpack_notify::show(&prompt);
    ExitCode::from(answer.exit_code())
}
