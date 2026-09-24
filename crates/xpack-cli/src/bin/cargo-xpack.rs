//! `cargo xpack`: packages a Cargo project with xPack.
//!
//! Cargo runs any `cargo-<name>` on the `PATH` as `cargo <name>`, so this
//! binary, shipped beside `xpack`, is the Rust integration: what the Maven
//! plugin is for a Maven project.
//!
//! # Glue, and nothing more
//!
//! It knows what Cargo knows (the package, its version, its binaries, where
//! they are built) and hands that to the `xpack` command line. It never
//! packages, signs, validates or installs anything itself: those have one
//! implementation, and a second one here would be where a security fix is
//! missed. Everything it writes, `xpack` checks, and reports in its own words.
//!
//! # Configuration
//!
//! ```toml
//! [package.metadata.xpack]
//! id = "com.example.mytool"      # required: the identity installations pin
//! name = "My Tool"               # default: the package name
//! publisher = "Example Ltd"      # default: the first author, without the address
//! command = "mytool"             # optional: typed by name in a terminal
//! commands = ["mytool-helper"]   # optional: further binaries, typed by name too
//! keep-working-directory = true  # default: true when there is a command
//! binaries = ["mytool"]          # default: the package's own binaries
//! launch = "mytool"              # default: the only, or first, of those
//! resources = ["assets"]         # copied into the payload, package-relative
//! icon = "assets/mytool.png"     # optional, package-relative, copied in
//! installer-ui = "installer-ui.json"
//! ```
//!
//! Each platform wants its own icon format, so `icon` may also name one per
//! platform; the one for the platform being built is used:
//!
//! ```toml
//! icon = { macos = "assets/mytool.icns", windows = "assets/mytool.ico", linux = "assets/mytool.png" }
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Args, Parser, Subcommand};
use serde::Deserialize;

/// Cargo calls this as `cargo-xpack xpack <args>`: the subcommand's own name
/// comes first.
#[derive(Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum Cargo {
    Xpack(Xpack),
}

/// Package a Cargo project with xPack.
#[derive(Args)]
#[command(version, about)]
struct Xpack {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Build the release binaries and pack them into a signed package.
    Pack(Common),
    /// Pack, then build the installer a user runs.
    Installer(InstallerArgs),
}

#[derive(Args)]
struct Common {
    /// Path to the package's `Cargo.toml`.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// The workspace member to package.
    #[arg(short, long, value_name = "SPEC")]
    package: Option<String>,

    /// The private signing key.
    #[arg(long, value_name = "FILE")]
    key: PathBuf,

    /// Where the package and the installer go. Default: `target/xpack`.
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,

    /// The `xpack` to run. Default: the one on the `PATH`.
    #[arg(long, value_name = "FILE", env = "XPACK")]
    xpack: Option<PathBuf>,
}

#[derive(Args)]
struct InstallerArgs {
    #[command(flatten)]
    common: Common,

    /// Build the console installer on Windows, for scripts.
    #[arg(long)]
    console: bool,
}

fn main() -> ExitCode {
    let Cargo::Xpack(args) = Cargo::parse();
    let result = match &args.action {
        Action::Pack(common) => pack(common).map(|packed| {
            xpack_core::outln!("{}", packed.display());
        }),
        Action::Installer(installer_args) => installer(installer_args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            xpack_core::errln!("cargo-xpack: {message}");
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, String>;

// --------------------------------------------------------------- what Cargo knows

/// The parts of `cargo metadata` used here.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    target_directory: PathBuf,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    description: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    manifest_path: PathBuf,
    targets: Vec<Target>,
    default_run: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

/// `[package.metadata.xpack]`.
#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Settings {
    id: Option<String>,
    name: Option<String>,
    publisher: Option<String>,
    command: Option<String>,
    #[serde(default)]
    commands: Vec<String>,
    keep_working_directory: Option<bool>,
    #[serde(default)]
    binaries: Vec<String>,
    launch: Option<String>,
    #[serde(default)]
    resources: Vec<PathBuf>,
    /// One path, or a table of them by platform: read by [`icon_for`], so a
    /// mistake is reported in words rather than as a failed match.
    icon: Option<serde_json::Value>,
    installer_ui: Option<PathBuf>,
}

/// The platforms `icon` may name, as `std::env::consts::OS` spells them.
const ICON_PLATFORMS: [&str; 3] = ["macos", "windows", "linux"];

/// The icon `setting` names for `os`: the one path, or `os`'s entry.
fn icon_for(setting: &serde_json::Value, os: &str) -> Result<Option<PathBuf>> {
    match setting {
        serde_json::Value::String(path) => Ok(Some(PathBuf::from(path))),
        serde_json::Value::Object(each) => {
            let mut chosen = None;
            for (platform, path) in each {
                if !ICON_PLATFORMS.contains(&platform.as_str()) {
                    return Err(format!(
                        "icon: {platform:?} is not a platform; use {}",
                        ICON_PLATFORMS.join(", ")
                    ));
                }
                let serde_json::Value::String(path) = path else {
                    return Err(format!("icon: {platform} must be a path"));
                };
                if platform == os {
                    chosen = Some(PathBuf::from(path));
                }
            }
            Ok(chosen)
        }
        _ => Err("icon must be a path, or a table of paths by platform".into()),
    }
}

/// Runs a command, failing with its own words when it fails.
fn run(command: &mut Command, what: &str) -> Result<std::process::Output> {
    let output = command.output().map_err(|e| format!("{what}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{what} failed:\n{}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output)
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn metadata(common: &Common) -> Result<Metadata> {
    let mut command = cargo();
    command.args(["metadata", "--format-version", "1", "--no-deps"]);
    if let Some(path) = &common.manifest_path {
        command.arg("--manifest-path").arg(path);
    }
    let output = run(&mut command, "cargo metadata")?;
    serde_json::from_slice(&output.stdout).map_err(|e| format!("reading cargo metadata: {e}"))
}

/// The package to build: the one named, or the only one, or the one whose
/// manifest is in the directory this was run from.
fn chosen<'a>(metadata: &'a Metadata, common: &Common) -> Result<&'a Package> {
    if let Some(name) = &common.package {
        return metadata
            .packages
            .iter()
            .find(|package| &package.name == name)
            .ok_or_else(|| format!("no package named {name} in this workspace"));
    }
    if let [only] = metadata.packages.as_slice() {
        return Ok(only);
    }
    let here = match &common.manifest_path {
        Some(path) => std::path::absolute(path).map_err(|e| e.to_string())?,
        None => std::env::current_dir().map_err(|e| e.to_string())?.join("Cargo.toml"),
    };
    metadata
        .packages
        .iter()
        .find(|package| package.manifest_path == here)
        .ok_or_else(|| "this is a workspace; name the package to build with -p".to_string())
}

fn settings(package: &Package) -> Result<Settings> {
    let Some(value) = package.metadata.as_ref().and_then(|m| m.get("xpack")) else {
        return Ok(Settings::default());
    };
    Settings::deserialize(value).map_err(|e| format!("[package.metadata.xpack]: {e}"))
}

// --------------------------------------------------------------- the work

/// What the package will contain, and how it starts.
struct Plan {
    settings: Settings,
    id: String,
    binaries: Vec<String>,
    launch: String,
    /// The icon for the platform being built, package-relative.
    icon: Option<PathBuf>,
    package_dir: PathBuf,
    staging: PathBuf,
    out_dir: PathBuf,
}

fn plan(metadata: &Metadata, package: &Package, common: &Common) -> Result<Plan> {
    let settings = settings(package)?;
    // Required, not derived: it is the identity every installation pins its
    // trust to, and a guess that changed with a rename would orphan them all.
    let id = settings.id.clone().ok_or_else(|| {
        format!(
            "{} has no [package.metadata.xpack] id; add one, e.g. id = \"com.example.{}\"",
            package.name, package.name
        )
    })?;
    let own: Vec<String> = package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
        .map(|target| target.name.clone())
        .collect();
    let binaries = if settings.binaries.is_empty() { own } else { settings.binaries.clone() };
    let launch = settings
        .launch
        .clone()
        .or_else(|| package.default_run.clone())
        .or_else(|| binaries.first().cloned())
        .ok_or_else(|| format!("{} has no binary to package", package.name))?;
    if !binaries.contains(&launch) {
        return Err(format!("launch = {launch:?} is not one of the binaries packaged"));
    }
    if let Some(extra) = settings.commands.iter().find(|name| !binaries.contains(name)) {
        return Err(format!("commands: {extra:?} is not one of the binaries packaged"));
    }
    let staging = metadata.target_directory.join("xpack").join(&package.name);
    let out_dir = common.out_dir.clone().unwrap_or_else(|| metadata.target_directory.join("xpack"));
    let package_dir = package.manifest_path.parent().map(Path::to_path_buf).unwrap_or_default();
    // Packages are built for this machine, so its icon is this machine's.
    let icon = match &settings.icon {
        Some(setting) => icon_for(setting, std::env::consts::OS)?,
        None => None,
    };
    let icon = icon.map(|path| inside_the_package(&path, "icon")).transpose()?;
    Ok(Plan { settings, id, binaries, launch, icon, package_dir, staging, out_dir })
}

/// `path`, when it names something inside the package directory.
fn inside_the_package(path: &Path, what: &str) -> Result<PathBuf> {
    if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
        return Err(format!("{what} {} must be a path inside the package", path.display()));
    }
    Ok(path.to_path_buf())
}

/// `cargo build --release` for exactly the binaries packaged.
///
/// Named by `--bin` from the workspace root, so a package may ship binaries
/// that other members of its workspace build.
fn build(metadata: &Metadata, plan: &Plan) -> Result<PathBuf> {
    let mut command = cargo();
    command.arg("build").arg("--release");
    command.arg("--manifest-path").arg(metadata.workspace_root.join("Cargo.toml"));
    for binary in &plan.binaries {
        command.arg("--bin").arg(binary);
    }
    run(&mut command, "cargo build --release")?;
    Ok(metadata.target_directory.join("release"))
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Lays out the payload: the binaries in `bin/`, the resources where the
/// package keeps them.
fn assemble(plan: &Plan, built: &Path) -> Result<PathBuf> {
    let payload = plan.staging.join("payload");
    if payload.exists() {
        std::fs::remove_dir_all(&payload).map_err(|e| format!("{}: {e}", payload.display()))?;
    }
    let bin = payload.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    for binary in &plan.binaries {
        let from = built.join(executable(binary));
        std::fs::copy(&from, bin.join(executable(binary)))
            .map_err(|e| format!("{}: {e}", from.display()))?;
    }
    for resource in &plan.settings.resources {
        let resource = inside_the_package(resource, "resource")?;
        copy_tree(&plan.package_dir.join(&resource), &payload.join(&resource))?;
    }
    // Only this platform's icon: the others would be dead weight in every
    // installation.
    if let Some(icon) = &plan.icon {
        copy_tree(&plan.package_dir.join(icon), &payload.join(icon))?;
    }
    Ok(payload)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let metadata = std::fs::metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
    if metadata.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
        for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::copy(from, to).map_err(|e| format!("{}: {e}", from.display()))?;
    }
    Ok(())
}

/// The first author, without the address: `Ada <ada@example.com>` is `Ada`.
fn publisher_of(package: &Package) -> Option<String> {
    let author = package.authors.first()?;
    let name = author.split('<').next().unwrap_or(author).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The `xpack.json` that describes the payload.
fn project_file(plan: &Plan, package: &Package) -> serde_json::Value {
    let settings = &plan.settings;
    let mut application = serde_json::json!({
        "id": plan.id,
        "name": settings.name.clone().unwrap_or_else(|| package.name.clone()),
        "version": package.version,
    });
    if let Some(description) = &package.description {
        application["description"] = description.trim().into();
    }
    if let Some(publisher) = settings.publisher.clone().or_else(|| publisher_of(package)) {
        application["publisher"] = publisher.into();
    }
    let mut launch =
        serde_json::json!({ "executable": format!("bin/{}", executable(&plan.launch)) });
    if settings.keep_working_directory.unwrap_or(settings.command.is_some()) {
        launch["keepWorkingDirectory"] = true.into();
    }
    let mut project = serde_json::json!({ "application": application, "launch": launch });
    if let Some(command) = &settings.command {
        project["command"] = serde_json::json!({ "name": command });
    }
    if !settings.commands.is_empty() {
        project["commands"] = settings
            .commands
            .iter()
            .map(|name| serde_json::json!({ "name": name, "executable": format!("bin/{}", executable(name)) }))
            .collect();
    }
    if let Some(icon) = &plan.icon {
        // The manifest names payload files with forward slashes everywhere.
        let icon: Vec<String> =
            icon.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        project["desktop"] = serde_json::json!({ "icon": icon.join("/") });
    }
    project
}

fn xpack(common: &Common) -> Command {
    Command::new(common.xpack.clone().unwrap_or_else(|| PathBuf::from("xpack")))
}

/// Builds the package and returns where it was written.
fn pack(common: &Common) -> Result<PathBuf> {
    let metadata = metadata(common)?;
    let package = chosen(&metadata, common)?;
    let plan = plan(&metadata, package, common)?;
    let built = build(&metadata, &plan)?;
    let payload = assemble(&plan, &built)?;

    let config = plan.staging.join("xpack.json");
    let text =
        serde_json::to_string_pretty(&project_file(&plan, package)).map_err(|e| e.to_string())?;
    std::fs::write(&config, text).map_err(|e| format!("{}: {e}", config.display()))?;
    std::fs::create_dir_all(&plan.out_dir)
        .map_err(|e| format!("{}: {e}", plan.out_dir.display()))?;

    let output = run(
        xpack(common)
            .arg("pack")
            .arg(&payload)
            .arg("--config")
            .arg(&config)
            .arg("--key")
            .arg(&common.key)
            .arg("--out-dir")
            .arg(&plan.out_dir)
            .arg("--json"),
        "xpack pack",
    )?;
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("reading xpack pack: {e}"))?;
    report["package"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "xpack pack did not say where the package is".to_string())
}

/// Packs, then builds the installer beside the package.
fn installer(args: &InstallerArgs) -> Result<()> {
    let common = &args.common;
    let package = pack(common)?;
    let metadata = metadata(common)?;
    let settings = settings(chosen(&metadata, common)?)?;
    let out_dir = common.out_dir.clone().unwrap_or_else(|| metadata.target_directory.join("xpack"));

    let mut command = xpack(common);
    command.arg("installer").arg(&package).arg("--out-dir").arg(&out_dir).arg("--json");
    if let Some(ui) = &settings.installer_ui {
        let package_dir = chosen(&metadata, common)?
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        command.arg("--ui").arg(package_dir.join(ui));
    }
    if args.console {
        command.arg("--console");
    }
    let output = run(&mut command, "xpack installer")?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("reading xpack installer: {e}"))?;
    xpack_core::outln!("{}", package.display());
    if let Some(installer) = report["installer"].as_str() {
        xpack_core::outln!("{installer}");
    }
    Ok(())
}
