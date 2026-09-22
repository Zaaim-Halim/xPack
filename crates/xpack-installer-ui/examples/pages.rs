//! Draws every page of the wizard, in both appearances, into image files.
//!
//! `cargo run -p xpack-installer-ui --features window --example pages -- DIR`
//!
//! For looking at the window where nothing may be put on the screen. Each
//! state is reached through the same model calls the window makes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xpack_core::{Error, ProgressEvent, ProgressReporter, Version};
use xpack_install::Existing;
use xpack_installer_ui::{
    Choices, Engine, Facts, Failure, FailureKind, Flavour, Inspection, Installed, Page, Step,
    UiPlan, Wizard, WizardSpec,
};

/// An engine that answers with a fixed verdict and installs nothing.
struct Fixed(Existing);

impl Engine for Fixed {
    fn inspect(&self, root: &Path) -> Inspection {
        Inspection { target: root.join("com.example.demo"), verdict: Ok(self.0.clone()) }
    }

    fn install(&self, _: &Choices, _: &dyn ProgressReporter) -> Result<Installed, Error> {
        Err(Error::invalid("example", "installs nothing"))
    }

    fn launch(&self, _: &Path) -> Result<(), Error> {
        Ok(())
    }
}

fn version(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

fn wizard(licence: bool, launch: bool) -> Wizard {
    Wizard::new(WizardSpec {
        flavour: Flavour::Mac,
        facts: Facts {
            name: "Demo App".into(),
            version: version("2.0.0"),
            publisher: Some("Example Publisher".into()),
            description: Some("A small application used to try the installer wizard.".into()),
        },
        plan: UiPlan { launch_on_finish: launch, ..UiPlan::default() },
        licence: licence.then(|| {
            "Demo licence.\n\nYou may use this demo for trying the installer.\n".repeat(12)
        }),
        shortcut_requested: true,
        root: PathBuf::from("/Users/demo/Applications/xPack"),
        root_fixed: false,
        log: Some(PathBuf::from("/tmp/xpack-installer.log")),
    })
}

/// A wizard walked forward to `page` against `existing`.
fn at(page: Page, existing: &Existing) -> Wizard {
    let mut wizard = wizard(true, true);
    let engine = Fixed(existing.clone());
    let root = wizard.root().to_path_buf();
    wizard.inspected(&root, engine.inspect(&root));
    while wizard.page() != page {
        if wizard.page() == Page::Licence {
            wizard.set_accepted(true);
        }
        match wizard.advance() {
            Step::Moved(_) | Step::Install(_) => {}
            Step::Refused => break,
        }
    }
    wizard
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("an output directory"));
    std::fs::create_dir_all(&out).expect("the output directory");
    let icon = std::fs::read(
        "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericApplicationIcon.icns",
    )
    .ok();

    let nothing = Existing::Nothing;
    let mut states: Vec<(&str, Wizard, Existing)> = vec![
        ("1-welcome", at(Page::Welcome, &nothing), nothing.clone()),
        ("2-licence", wizard_on_licence(), nothing.clone()),
        ("3-location-new", at(Page::Location, &nothing), nothing.clone()),
        (
            "3-location-upgrade",
            at(Page::Location, &Existing::Older(version("1.4.2"))),
            nothing.clone(),
        ),
        (
            "3-location-newer",
            at(Page::Location, &Existing::Newer(version("3.0.0"))),
            nothing.clone(),
        ),
        ("3-location-busy", at(Page::Location, &Existing::Busy), nothing.clone()),
        ("3-location-installed", at(Page::Location, &Existing::Installed), nothing.clone()),
        ("4-ready", at(Page::Ready, &nothing), nothing.clone()),
    ];

    let mut installing = at(Page::Ready, &nothing);
    installing.advance();
    installing.observe(&ProgressEvent::ExtractionProgress {
        files_completed: 180,
        files_total: 250,
        bytes_completed: 36 * 1_048_576,
        bytes_total: 50 * 1_048_576,
    });
    states.push(("5-installing", installing, nothing.clone()));

    let mut done = at(Page::Ready, &nothing);
    done.advance();
    done.finished(Ok(Installed {
        directory: PathBuf::from("/Users/demo/Applications/xPack/com.example.demo"),
        version: version("2.0.0"),
        shortcut_added: true,
    }));
    states.push(("6-finish", done, nothing.clone()));

    let mut failed = at(Page::Ready, &nothing);
    failed.advance();
    failed.finished(Err(Failure {
        kind: FailureKind::Other,
        message: "i/o error at /Users/demo/Applications/xPack: permission denied".into(),
    }));
    states.push(("6-finish-failed", failed, nothing));

    for (name, wizard, existing) in states {
        for (suffix, dark) in [("light", false), ("dark", true)] {
            let engine: Arc<dyn Engine> = Arc::new(Fixed(existing.clone()));
            let image =
                xpack_installer_ui::window::snapshot(wizard.clone(), engine, icon.as_deref(), dark)
                    .expect("a snapshot");
            let path = out.join(format!("{name}-{suffix}.tiff"));
            std::fs::write(&path, image).expect("the image");
            println!("{}", path.display());
        }
    }
}

fn wizard_on_licence() -> Wizard {
    let mut wizard = wizard(true, true);
    wizard.advance();
    wizard
}
