//! An installation running on its own thread, reporting to the wizard.
//!
//! The window must keep drawing while the installer works, so the work runs
//! elsewhere and hands back what happened. Both front-ends use this; neither
//! touches a thread or a lock of its own.

use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use xpack_core::{ProgressEvent, ProgressReporter};

use crate::engine::{Choices, Engine, Failure, FailureKind, Installed};
use crate::model::Wizard;

/// Something that wakes the window's thread, so it collects what arrived.
pub type Wake = Box<dyn Fn() + Send + Sync>;

/// What the worker has reported and the window has not collected yet.
#[derive(Default)]
struct Shared {
    events: Vec<ProgressEvent>,
    result: Option<Result<Installed, Failure>>,
}

/// An installation in progress.
pub struct Installation {
    shared: Arc<Mutex<Shared>>,
    worker: Option<JoinHandle<()>>,
}

impl Installation {
    /// Starts installing on a worker thread.
    ///
    /// `wake` is called after each event and once at the end, from the worker.
    /// It must only nudge the window's thread; the window then calls
    /// [`Self::deliver`] there.
    pub fn start(engine: Arc<dyn Engine>, choices: Choices, wake: Wake) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let reporter = Reporter { shared: Arc::clone(&shared), wake };
        let worker = std::thread::spawn(move || {
            // An installer that panicked has not finished, and a wizard
            // waiting for a result that never comes would refuse to close
            // for ever. So a panic becomes a failure like any other.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                engine.install(&choices, &reporter).map_err(|error| Failure::of(&error))
            }))
            .unwrap_or_else(|_| {
                Err(Failure {
                    kind: FailureKind::Other,
                    message: "the installer stopped unexpectedly".to_string(),
                })
            });
            lock(&reporter.shared).result = Some(outcome);
            (reporter.wake)();
        });
        Self { shared, worker: Some(worker) }
    }

    /// Hands everything reported so far to the wizard.
    ///
    /// Returns `true` once the result has been delivered: the installation
    /// is over and the worker has finished.
    pub fn deliver(&mut self, wizard: &mut Wizard) -> bool {
        let (events, result) = {
            let mut shared = lock(&self.shared);
            (std::mem::take(&mut shared.events), shared.result.take())
        };
        for event in &events {
            wizard.observe(event);
        }
        let Some(result) = result else {
            return false;
        };
        wizard.finished(result);
        if let Some(worker) = self.worker.take() {
            // It has already stored its result; all that is left is to end.
            let _ = worker.join();
        }
        true
    }
}

/// The worker's side: collects events and wakes the window.
struct Reporter {
    shared: Arc<Mutex<Shared>>,
    wake: Wake,
}

impl ProgressReporter for Reporter {
    fn report(&self, event: &ProgressEvent) {
        lock(&self.shared).events.push(event.clone());
        (self.wake)();
    }
}

/// Takes the lock even after a panic elsewhere: the data is a queue of plain
/// values that cannot be left half-updated, and progress must never panic.
fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use xpack_core::{Error, Version};
    use xpack_install::Existing;

    use super::*;
    use crate::engine::Inspection;
    use crate::model::{Facts, Flavour, Page, Step, UiPlan, WizardSpec};

    /// An engine that reports two events and then answers as told.
    struct Scripted {
        answer: fn() -> Result<Installed, Error>,
    }

    impl Engine for Scripted {
        fn inspect(&self, root: &Path) -> Inspection {
            Inspection { target: root.join("app"), verdict: Ok(Existing::Nothing) }
        }

        fn install(
            &self,
            _choices: &Choices,
            progress: &dyn ProgressReporter,
        ) -> Result<Installed, Error> {
            let version = Version::parse("2.0.0").unwrap();
            progress.report(&ProgressEvent::Installing { version: version.clone() });
            progress.report(&ProgressEvent::ExtractionProgress {
                files_completed: 1,
                files_total: 2,
                bytes_completed: 5,
                bytes_total: 10,
            });
            (self.answer)()
        }

        fn launch(&self, _root: &Path) -> Result<(), Error> {
            Ok(())
        }
    }

    #[allow(clippy::unnecessary_wraps)] // Matches the engine's signature it stands in for.
    fn installed() -> Result<Installed, Error> {
        Ok(Installed {
            directory: PathBuf::from("/r/app"),
            version: Version::parse("2.0.0").unwrap(),
            shortcut_added: false,
        })
    }

    /// A wizard at the moment its installation starts, and its choices.
    fn starting(engine: &dyn Engine) -> (Wizard, Choices) {
        let mut wizard = Wizard::new(WizardSpec {
            flavour: Flavour::Mac,
            facts: Facts {
                name: "App".into(),
                version: Version::parse("2.0.0").unwrap(),
                publisher: None,
                description: None,
            },
            plan: UiPlan::default(),
            licence: None,
            shortcut_requested: false,
            root: PathBuf::from("/r"),
            root_fixed: false,
            log: None,
        });
        wizard.inspected(Path::new("/r"), engine.inspect(Path::new("/r")));
        loop {
            match wizard.advance() {
                Step::Moved(_) => {}
                Step::Install(choices) => return (wizard, choices),
                Step::Refused => panic!("stuck on {:?}", wizard.page()),
            }
        }
    }

    /// Delivers until the installation ends, as a window's loop would.
    fn run_to_the_end(installation: &mut Installation, wizard: &mut Wizard) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !installation.deliver(wizard) {
            assert!(Instant::now() < deadline, "the installation never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn progress_and_the_result_reach_the_wizard() {
        let engine = Arc::new(Scripted { answer: installed });
        let (mut wizard, choices) = starting(engine.as_ref());
        let mut installation = Installation::start(engine, choices, Box::new(|| {}));

        run_to_the_end(&mut installation, &mut wizard);

        assert_eq!(wizard.page(), Page::Finish);
        assert_eq!(wizard.progress().permille(), Some(500));
        assert!(wizard.finish_view().unwrap().succeeded);
    }

    #[test]
    fn a_failure_reaches_the_wizard_classified() {
        let engine = Arc::new(Scripted { answer: || Err(Error::Integrity("bad".into())) });
        let (mut wizard, choices) = starting(engine.as_ref());
        let mut installation = Installation::start(engine, choices, Box::new(|| {}));

        run_to_the_end(&mut installation, &mut wizard);

        let view = wizard.finish_view().unwrap();
        assert_eq!(view.title, "This installer is damaged and can't be used.");
    }

    #[test]
    fn an_installer_that_panics_still_ends_the_wait() {
        // Otherwise the wizard would refuse to close for ever.
        let engine = Arc::new(Scripted { answer: || panic!("a bug in the installer") });
        let (mut wizard, choices) = starting(engine.as_ref());
        let mut installation = Installation::start(engine, choices, Box::new(|| {}));

        run_to_the_end(&mut installation, &mut wizard);

        assert_eq!(wizard.page(), Page::Finish);
        assert!(!wizard.finish_view().unwrap().succeeded);
    }

    #[test]
    fn the_window_is_woken_for_each_event_and_the_end() {
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let engine = Arc::new(Scripted { answer: installed });
        let (mut wizard, choices) = starting(engine.as_ref());
        let mut installation = Installation::start(
            engine,
            choices,
            Box::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );

        run_to_the_end(&mut installation, &mut wizard);
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
