//! Launching the payload application.
//!
//! xPack does not know or care what the payload is. The manifest declares an
//! executable and its arguments; this module runs exactly that. There is no
//! branch anywhere on "is this Java", which is what keeps the engine generic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use xpack_core::{Error, LaunchSpec, Result};

/// Everything needed to start the payload.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    /// Directory of the version being launched. Relative paths resolve here.
    pub version_dir: PathBuf,
    /// Launch description taken from the signed manifest.
    pub spec: LaunchSpec,
    /// Arguments supplied by the user, appended after the manifest's own.
    pub user_arguments: Vec<String>,
    /// Whether the child keeps this process's standard streams.
    pub inherit_stdio: bool,
    /// Environment the launcher adds, on top of the manifest's.
    ///
    /// Kept separate from the manifest's own entries because these are set by
    /// xPack rather than by the publisher, and a manifest must not be able to
    /// overwrite them: they tell the application how to talk back to the thing
    /// that started it.
    pub launcher_environment: Vec<(String, String)>,
}

impl LaunchRequest {
    /// Builds a request for `version_dir` using a manifest's launch spec.
    pub fn new(version_dir: impl Into<PathBuf>, spec: LaunchSpec) -> Self {
        Self {
            version_dir: version_dir.into(),
            spec,
            user_arguments: Vec::new(),
            inherit_stdio: true,
            launcher_environment: Vec::new(),
        }
    }

    /// Adds an environment entry the manifest cannot override.
    #[must_use]
    pub fn with_launcher_environment(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.launcher_environment.push((key.into(), value.into()));
        self
    }

    /// Appends the arguments the user typed.
    #[must_use]
    pub fn with_user_arguments(mut self, arguments: Vec<String>) -> Self {
        self.user_arguments = arguments;
        self
    }
}

/// Resolves the executable a launch spec names.
///
/// A value containing a separator is a **bundled** runtime and resolves inside
/// the version directory. A bare name is a **system** runtime and is left for
/// the operating system to find on `PATH`. Both are supported deliberately:
/// bundling gives predictable behaviour, while a system runtime gives a much
/// smaller package.
///
/// The bundled form is checked for existence here so the failure says "the
/// package is missing this file" rather than surfacing later as a bare
/// `NotFound` from the process spawn.
pub fn resolve_executable(version_dir: &Path, spec: &LaunchSpec) -> Result<PathBuf> {
    // A manifest cannot carry an empty executable, but this is a public entry
    // point and an empty or blank name would otherwise reach the spawn and
    // fail as a bare "no such file", naming nothing useful.
    if spec.executable.trim().is_empty() {
        return Err(Error::Launch("the launch executable is empty".to_string()));
    }

    if !spec.is_bundled() {
        return Ok(PathBuf::from(&spec.executable));
    }

    // Apply the same rule the manifest enforces, here at the point of use.
    // A signed manifest cannot carry a traversing executable, but this is a
    // public entry point, and the function that decides which binary to run
    // must not depend on a validator that ran somewhere else, earlier, on
    // input it cannot see.
    xpack_core::manifest::validate_relative_path("launch.executable", &spec.executable)?;

    let relative = spec.executable.replace('\\', "/");
    let mut resolved = version_dir.to_path_buf();
    for component in relative.split('/') {
        resolved.push(component);
    }

    // Belt and braces: confirm the result really is inside the version
    // directory once symbolic links have been followed. The check above
    // rejects traversal in the *name*; this catches a link inside the payload
    // that points out of the installation.
    if let (Ok(real), Ok(root)) = (resolved.canonicalize(), version_dir.canonicalize())
        && !real.starts_with(&root)
    {
        return Err(Error::Launch(format!(
            "{} resolves outside the installed version directory",
            spec.executable
        )));
    }

    if !resolved.is_file() {
        return Err(Error::Launch(format!(
            "{} does not exist; the installed version is incomplete",
            resolved.display()
        )));
    }
    Ok(resolved)
}

/// Starts the payload and returns the child process.
///
/// The user's arguments are appended after the manifest's and passed through
/// unchanged. The launcher never rewrites, reorders or filters them — an
/// application that receives different arguments than the user typed is an
/// application whose behaviour cannot be reasoned about.
///
/// The manifest's own arguments, and its environment values, are the
/// publisher's: in those, and only those, [`VERSION_DIR_PLACEHOLDER`] becomes
/// the absolute path of the version being started.
///
/// The child **inherits this process's environment**, with the manifest's
/// entries added on top. The environment is not cleared, because a payload
/// generally needs the user's `PATH`, locale and display settings to work at
/// all. A manifest entry overrides an inherited one of the same name.
///
/// [`VERSION_DIR_PLACEHOLDER`]: xpack_core::VERSION_DIR_PLACEHOLDER
pub fn launch(request: &LaunchRequest) -> Result<Child> {
    let executable = resolve_executable(&request.version_dir, &request.spec)?;

    // Absolute, because the application may start somewhere else entirely: a
    // relative installation root would otherwise name a path that means
    // nothing from the application's own directory.
    let version_dir = std::path::absolute(&request.version_dir)
        .map_err(|e| Error::Launch(format!("{}: {e}", request.version_dir.display())))?;

    let mut command = Command::new(&executable);
    command.args(request.spec.arguments_in(&version_dir));
    command.args(&request.user_arguments);

    // `None` leaves the child in this process's directory, which is the one
    // the user invoked the application from.
    let working_directory = working_directory(request)?;
    if let Some(directory) = &working_directory {
        command.current_dir(directory);
    }

    for (key, value) in request.spec.environment_in(&version_dir) {
        command.env(key, value);
    }
    // Applied last, so a manifest cannot shadow the variables xPack uses to
    // hear back from the application it started.
    for (key, value) in &request.launcher_environment {
        command.env(key, value);
    }

    if request.inherit_stdio {
        command.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    }

    tracing::info!(
        executable = %executable.display(),
        arguments = request.spec.arguments.len() + request.user_arguments.len(),
        working_directory = %working_directory
            .as_deref()
            .map_or_else(|| "(where it was invoked)".into(), |d| d.display().to_string()),
        "launching application"
    );

    command
        .spawn()
        .map_err(|e| Error::Launch(format!("could not start {}: {e}", executable.display())))
}

/// Resolves the working directory, defaulting to the version directory.
///
/// `None` means the one this process was started in, when the manifest asks
/// to keep it.
fn working_directory(request: &LaunchRequest) -> Result<Option<PathBuf>> {
    if request.spec.keep_working_directory {
        // A validated manifest cannot say both; a spec built by hand can, and
        // silently picking one would start the application somewhere its
        // author did not expect.
        if request.spec.working_directory.is_some() {
            return Err(Error::Launch(
                "launch.workingDirectory and launch.keepWorkingDirectory contradict each other"
                    .into(),
            ));
        }
        return Ok(None);
    }
    let Some(relative) = &request.spec.working_directory else {
        return Ok(Some(request.version_dir.clone()));
    };

    let mut resolved = request.version_dir.clone();
    for component in relative.replace('\\', "/").split('/') {
        resolved.push(component);
    }
    if !resolved.is_dir() {
        return Err(Error::Launch(format!(
            "working directory {} does not exist",
            resolved.display()
        )));
    }
    Ok(Some(resolved))
}

/// Builds a launch spec from its parts, for callers without a manifest.
pub fn launch_spec(
    executable: impl Into<String>,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
) -> LaunchSpec {
    LaunchSpec {
        executable: executable.into(),
        arguments,
        working_directory: None,
        keep_working_directory: false,
        environment,
    }
}

/// Asks a process to close itself, the way a user clicking its close button would.
///
/// # Asking, not killing
///
/// The application being asked is the user's, and it may have unsaved work. So
/// this sends the request every desktop platform already has for "please
/// close": the application's own shutdown runs, its "do you want to save?"
/// prompt appears if it has one, and it is free to refuse. A refusal is not a
/// failure here — it means the user said no, and the update simply waits for
/// the next start, which is what it would have done anyway.
///
/// Returning `Ok` therefore means the request was delivered, never that the
/// process has exited. The caller waits for that separately.
///
/// # Why an operating-system command rather than an API call
///
/// The direct calls are `PostMessage(WM_CLOSE)` on Windows and `kill(2)` on
/// Unix, and both are FFI that this workspace forbids. `taskkill` without
/// `/F` posts exactly that message, and `kill` sends exactly that signal; they
/// ship with the operating system and are older than most of the code that
/// would call them. Trading one process spawn for keeping the workspace free
/// of unsafe is a trade this crate has made before.
///
/// **`/F` is never passed.** It terminates without asking, which is precisely
/// the behaviour that loses a user's work.
///
/// # A Windows process with no window cannot be asked
///
/// `WM_CLOSE` is posted to the process's windows, so a console program that
/// has none has nothing to receive it, and this reports a failure rather than
/// ending it. That is the right answer — the alternative is `/F` — but it
/// means a windowless application on Windows cannot be restarted by asking.
///
/// In practice the applications this is used on have windows: it is called
/// when a user has clicked a button in a dialog, which an application without
/// a window cannot have shown them. Where it does happen, the caller learns
/// from the error, the application keeps running, and the update is applied at
/// the next start like any other.
pub fn request_close(pid: u32) -> Result<()> {
    let mut command = close_command(pid);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

    let status = command
        .status()
        .map_err(|e| Error::Launch(format!("asking process {pid} to close: {e}")))?;

    if status.success() {
        return Ok(());
    }
    // A non-zero status usually means the process had already exited, which is
    // the outcome the caller wanted anyway. It is reported rather than hidden,
    // because a caller that asked twice would otherwise never learn why.
    Err(Error::Launch(format!("process {pid} did not accept a close request (status {status})")))
}

/// The command that asks a process to close, for this platform.
#[cfg(windows)]
fn close_command(pid: u32) -> Command {
    let mut command = Command::new("taskkill");
    // No `/F`: this posts WM_CLOSE to the process's windows and lets the
    // application decide what to do about it.
    command.arg("/PID").arg(pid.to_string());
    command
}

/// The command that asks a process to close, for this platform.
#[cfg(not(windows))]
fn close_command(pid: u32) -> Command {
    let mut command = Command::new("kill");
    // SIGTERM, the signal a desktop application is expected to handle by
    // shutting down cleanly. Never SIGKILL, which it cannot handle at all.
    command.arg("-TERM").arg(pid.to_string());
    command
}
