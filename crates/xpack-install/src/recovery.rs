//! Finishing or undoing whatever the last run left behind.
//!
//! Recovery runs at the start of every operation, driven entirely by the
//! persisted update phase. Scattering "is this left over?" checks through the
//! installer would guarantee they disagree, so the rules live in one table.
//!
//! | Phase | Action |
//! | --- | --- |
//! | `Idle` | nothing |
//! | `Downloading`, `Verifying` | discard partial downloads |
//! | `Installing` | discard the staging tree, and the version directory *only* if state does not know it |
//! | `Staged` | leave it — extraction completed, the version is activatable |
//! | `RollingBack` | finish the rollback |
//! | `PendingVerification` | leave it — only the launcher may resolve probation |
//!
//! The `Installing` rule is the subtle one. A version directory that state
//! already records is a real installed version and must never be removed by
//! recovery; one state has never heard of is debris from a crash between
//! promotion and the state write.

use xpack_core::atomic;
use xpack_core::state::UpdatePhase;
use xpack_core::{Result, Version};
use xpack_platform::InstallLock;

/// What recovery did, for logging and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Partial downloads removed.
    pub cleared_downloads: bool,
    /// Staging trees removed.
    pub removed_staging: Vec<String>,
    /// Version directories removed as crash debris.
    pub removed_debris: Vec<String>,
    /// A rollback that was completed.
    pub completed_rollback: Option<String>,
    /// Whether the phase was left untouched deliberately.
    pub left_in_place: bool,
}

impl RecoveryReport {
    /// Returns `true` when recovery changed nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Resolves whatever the previous run left behind.
pub fn recover(lock: &InstallLock) -> Result<RecoveryReport> {
    let paths = lock.paths();
    let mut report = RecoveryReport::default();

    let Some(id) = paths.application_id() else {
        return Ok(report);
    };
    let mut state = lock.load_or_new_state(id)?;

    match state.update.clone() {
        UpdatePhase::Idle => return Ok(report),

        UpdatePhase::Downloading { .. } | UpdatePhase::Verifying { .. } => {
            atomic::remove_dir_all_if_exists(&paths.downloads_dir())?;
            report.cleared_downloads = true;
        }

        UpdatePhase::Installing { version } => {
            atomic::remove_dir_all_if_exists(&paths.staging_dir(&version))?;
            report.removed_staging.push(version.to_string());

            // Only debris state has never recorded may be removed.
            if state.record(&version).is_none() {
                let dir = paths.version_dir(&version);
                if dir.exists() {
                    atomic::remove_dir_all_if_exists(&dir)?;
                    report.removed_debris.push(version.to_string());
                }
            }
        }

        UpdatePhase::Staged { .. } => {
            // Extraction finished. The version is complete and activatable, so
            // discarding it would throw away good work for no reason.
            report.left_in_place = true;
        }

        UpdatePhase::RollingBack { from, to } => {
            finish_rollback(lock, &mut state, &from, &to)?;
            report.completed_rollback = Some(to.to_string());
        }

        UpdatePhase::PendingVerification { .. } => {
            // A version on probation is running, or was. Only the launcher can
            // decide whether probation passed; recovery must not pre-empt it.
            report.left_in_place = true;
            return Ok(report);
        }
    }

    state.update = UpdatePhase::Idle;
    lock.save_state(&state)?;
    tracing::info!(?report, "recovered from an interrupted operation");
    Ok(report)
}

/// Completes a rollback that was interrupted part way through.
fn finish_rollback(
    lock: &InstallLock,
    state: &mut xpack_core::InstallState,
    from: &Version,
    to: &Version,
) -> Result<()> {
    state.mark_bad(from, "an update was rolled back");
    state.current_version = Some(to.clone());
    state.update = UpdatePhase::Idle;
    lock.save_state(state)?;
    xpack_platform::update_current_link(lock.paths(), to).log();
    Ok(())
}
