//! An update applied by the updater, as `xpack update` applies one: the new
//! version's update hooks run around its activation, with the installation's
//! own `xpack-hook`, and what they say reaches whoever watches the progress.
//!
//! These run the real `xpack-hook` from the workspace's build, and are
//! skipped, saying so, when it has not been built; CI builds it first.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::progress::{ProgressEvent, ProgressReporter};
use xpack_core::{InstallPaths, Manifest, Platform, Result, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;
use xpack_update::{UpdateOptions, UpdateTransport, Updater};

const BASE: &str = "https://updates.example.com/demo";

fn engine() -> Option<PathBuf> {
    let test = std::env::current_exe().unwrap();
    let engine =
        test.parent()?.parent()?.join(format!("xpack-hook{}", std::env::consts::EXE_SUFFIX));
    if engine.is_file() {
        return Some(engine);
    }
    assert!(std::env::var_os("CI").is_none(), "{} is not built", engine.display());
    eprintln!("skipped: {} is not built; run `cargo build --workspace` first", engine.display());
    None
}

/// Serves files by URL.
#[derive(Default)]
struct Server(Mutex<BTreeMap<String, Vec<u8>>>);

impl UpdateTransport for Server {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64> {
        let routes = self.0.lock().unwrap();
        let bytes = routes
            .get(url)
            .ok_or_else(|| xpack_core::Error::Transport(format!("HTTP 404 for {url}")))?;
        let bytes = &bytes[..bytes.len().min(usize::try_from(limit).unwrap_or(usize::MAX))];
        sink.write_all(bytes).map_err(|e| xpack_core::Error::Transport(e.to_string()))?;
        Ok(bytes.len() as u64)
    }
}

/// Appends `<point>;` to `dataDir/marks`, and says so.
const MARK: &str = "export function main(ctx) {
    const file = ctx.path(ctx.dataDir, 'marks');
    const before = ctx.file.exists(file) ? ctx.file.read(file) : '';
    ctx.file.write(file, before + ctx.operation + '.' + ctx.when + ';');
    ctx.log.info('marked ' + ctx.fromVersion + ' -> ' + ctx.toVersion);
}";

fn package(dir: &Path, key: &KeyPair, version: &str, hooks: serde_json::Value) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::create_dir_all(payload.join("xpack/hooks")).unwrap();
    std::fs::write(payload.join("bin/app"), "#!/bin/sh\n").unwrap();
    std::fs::write(payload.join("xpack/hooks/mark.js"), MARK).unwrap();
    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
        instance: xpack_core::InstanceSpec::default(),
        hooks: serde_json::from_value(hooks).unwrap(),
    };
    let manifest = Manifest { format_version: manifest.required_format_version(), ..manifest };
    let out = dir.join(format!("demo-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// Every hook line reported, as `[point] line`.
#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl ProgressReporter for Lines {
    fn report(&self, event: &ProgressEvent) {
        if let ProgressEvent::HookOutput { point, line } = event {
            self.0.lock().unwrap().push(format!("[{point}] {line}"));
        }
    }
}

#[test]
fn an_update_applied_by_the_updater_runs_its_update_hooks_and_reports_what_they_say() {
    let Some(engine) = engine() else { return };
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.demo").unwrap();
    {
        // The first version has no hooks; the installation gets the engine
        // anyway, and the update is what brings hooks.
        let first = package(dir.path(), &key, "1.0.0", serde_json::json!({}));
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut verified =
            open_and_verify(&first, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        let options =
            InstallOptions { activate: true, hook_engine: Some(engine), ..Default::default() };
        Installer::new(&lock).install(&mut verified, &options).unwrap();
    }
    let hooks = serde_json::json!({ "update": [
        { "when": "before", "script": "xpack/hooks/mark.js" },
        { "when": "after", "script": "xpack/hooks/mark.js" }
    ]});
    let update = package(dir.path(), &key, "1.1.0", hooks);
    let server = Server::default();
    let index = format!(
        r#"{{"application":"com.example.demo","version":"1.1.0",
            "platform":{{"os":"{}","arch":"{}"}},"channel":"stable",
            "package":{{"file":"demo-1.1.0.xpkg","size":{}}}}}"#,
        Platform::host().unwrap().os,
        Platform::host().unwrap().arch,
        std::fs::metadata(&update).unwrap().len()
    );
    server.0.lock().unwrap().insert(format!("{BASE}/stable.json"), index.into_bytes());
    server
        .0
        .lock()
        .unwrap()
        .insert(format!("{BASE}/demo-1.1.0.xpkg"), std::fs::read(&update).unwrap());

    let lines = Lines::default();
    let options = UpdateOptions { activate: true, ..Default::default() };
    let installed =
        Updater::new(&paths, &server).reporting_to(&lines).update(BASE, &options).unwrap();

    assert_eq!(installed, Some(Version::parse("1.1.0").unwrap()));
    let marks = std::fs::read_to_string(paths.data_dir().join("marks")).unwrap_or_default();
    assert_eq!(marks, "update.before;update.after;");
    assert_eq!(
        *lines.0.lock().unwrap(),
        ["[update.before] marked 1.0.0 -> 1.1.0", "[update.after] marked 1.0.0 -> 1.1.0"]
    );
}
