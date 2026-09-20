//! Staged rollout: offering a release to a fraction of installations first.
//!
//! Shipping a new version to every user the moment it is published is the
//! failure mode that a rollback cannot save you from. xPack already rolls an
//! individual machine back when a version fails to start, but that only helps
//! the machines that noticed — a release which is broken in a way the startup
//! check cannot see reaches everyone at once, and there is no brake.
//!
//! A staged rollout is the brake. The publisher offers 1.1.0 to 5% of
//! installations, watches, and raises the number. If it is bad, they set it
//! back to 0 and nobody else receives it.
//!
//! # Why the client decides, not the server
//!
//! Chrome's Omaha assigns cohorts server-side, which needs a server that runs
//! code. An xPack update server is **static files** — an index and some
//! packages, served by anything that can serve a file, including object
//! storage with no compute at all. Keeping it that way is worth more than
//! server-side control, so the decision is made on the client: the index
//! declares a percentage and each installation works out for itself whether it
//! is inside it.
//!
//! That is what Sparkle and electron-updater do, for the same reason.
//!
//! # The properties that make it a brake rather than a lottery
//!
//! **Stable.** An installation offered a version stays offered it. Without
//! this an update would appear and disappear between checks, and a user who
//! saw it once could never get it again.
//!
//! **Monotonic.** Raising the percentage only ever adds installations. This is
//! what makes "5%, then 25%, then 100%" a rollout rather than three unrelated
//! random draws, and it is why the bucket is compared with `<` against a
//! stable number rather than re-drawn each time.
//!
//! **Rotating.** The cohort is derived from the installation *and the version*,
//! so the same unlucky machines are not first for every single release.
//! electron-updater keys on the installation alone, which makes a fixed
//! population the permanent guinea pigs; including the version costs nothing
//! and spreads the exposure.
//!
//! # Nothing is transmitted
//!
//! The identifier is random, generated locally, stored in the installation's
//! own state and never sent anywhere — the client fetches a static file and
//! decides in private. There is no cohort to report and no server to report it
//! to, so a staged rollout here reveals nothing about a user.

use xpack_core::Version;

/// Percentage of installations a release is offered to.
///
/// `0` halts a rollout: nobody is offered the version, which is how a
/// publisher stops a bad release from spreading further. `100` is a normal,
/// fully published release, and is what an index without a rollout means.
pub const FULLY_ROLLED_OUT: u8 = 100;

/// Interprets the index's declared percentage.
///
/// The index is not a trust input and its values are attacker-controlled, so a
/// nonsense percentage must not break updating. Anything above 100 is treated
/// as a full rollout, which is the same thing it would mean if honest, and a
/// missing value means the release is fully published — the only reading that
/// keeps every index written before staged rollouts existed working.
pub fn declared_percentage(rollout: Option<u8>) -> u8 {
    rollout.map_or(FULLY_ROLLED_OUT, |value| value.min(FULLY_ROLLED_OUT))
}

/// Returns `true` when this installation is inside the staged rollout.
///
/// `rollout_id` is the installation's stable random identifier and `version`
/// the release being offered. See the module documentation for the three
/// properties this guarantees.
pub fn is_in_rollout(rollout_id: &str, version: &Version, percentage: u8) -> bool {
    bucket_of(rollout_id, version) < percentage.min(FULLY_ROLLED_OUT)
}

/// Which of the hundred buckets an installation falls in for a version.
///
/// Derived from a cryptographic hash rather than a cheaper one so the spread
/// is even without having to reason about the identifier's own distribution.
/// Nothing here is a security boundary — an installation that lied about its
/// bucket would only change which updates it is offered, which it can already
/// do by updating manually.
fn bucket_of(rollout_id: &str, version: &Version) -> u8 {
    let digest = xpack_security::sha256(format!("{rollout_id}:{version}").as_bytes());
    let bytes = digest.as_bytes();

    // Eight bytes is far more than the hundred buckets need; taking a whole
    // u64 and reducing keeps the modulo bias immeasurably small rather than
    // relying on a single byte, where 256 does not divide by 100 evenly.
    let value = u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    u8::try_from(value % u64::from(FULLY_ROLLED_OUT)).unwrap_or(0)
}

/// Generates a new random installation identifier.
///
/// 128 bits, hex encoded. It identifies nothing about the machine or the user:
/// it exists only so that the bucket is stable across checks, and two
/// installations of the same application on one machine are independent.
pub fn new_rollout_id() -> xpack_core::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| {
        xpack_core::Error::Unsupported(format!("no source of randomness available: {e}"))
    })?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn ids(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("{i:032x}")).collect()
    }

    #[test]
    fn a_full_rollout_reaches_every_installation() {
        let version = v("1.1.0");
        for id in ids(500) {
            assert!(is_in_rollout(&id, &version, 100), "{id} was excluded from a full rollout");
        }
    }

    #[test]
    fn a_halted_rollout_reaches_nobody() {
        // Setting the percentage to zero is how a publisher stops a bad
        // release spreading any further.
        let version = v("1.1.0");
        for id in ids(500) {
            assert!(!is_in_rollout(&id, &version, 0), "{id} received a halted rollout");
        }
    }

    #[test]
    fn the_decision_is_stable_across_repeated_checks() {
        // An update that appeared and disappeared between checks would be
        // worse than one that never appeared.
        let version = v("1.1.0");
        for id in ids(100) {
            let first = is_in_rollout(&id, &version, 37);
            for _ in 0..5 {
                assert_eq!(is_in_rollout(&id, &version, 37), first, "{id} flapped");
            }
        }
    }

    #[test]
    fn raising_the_percentage_only_ever_adds_installations() {
        // This is what makes 5% -> 25% -> 100% a rollout rather than three
        // unrelated draws. An installation that was offered a version must
        // never stop being offered it.
        let version = v("1.1.0");
        for id in ids(300) {
            let mut was_in = false;
            for percentage in 0..=100u8 {
                let is_in = is_in_rollout(&id, &version, percentage);
                assert!(
                    !was_in || is_in,
                    "{id} was in the rollout and then dropped out at {percentage}%"
                );
                was_in = is_in;
            }
        }
    }

    #[test]
    fn the_share_offered_is_close_to_the_percentage_asked_for() {
        let version = v("1.1.0");
        let population = ids(10_000);
        for percentage in [5u8, 25, 50, 90] {
            let count =
                population.iter().filter(|id| is_in_rollout(id, &version, percentage)).count();
            let share = count * 100 / population.len();
            let asked = usize::from(percentage);
            assert!(
                share.abs_diff(asked) <= 2,
                "asked for {asked}%, offered to {share}% of installations"
            );
        }
    }

    #[test]
    fn a_different_version_reassigns_the_cohort() {
        // Keying on the installation alone would make one fixed population the
        // permanent early adopters for every release.
        let population = ids(1_000);
        let first: Vec<bool> =
            population.iter().map(|id| is_in_rollout(id, &v("1.1.0"), 25)).collect();
        let second: Vec<bool> =
            population.iter().map(|id| is_in_rollout(id, &v("1.2.0"), 25)).collect();

        let same = first.iter().zip(&second).filter(|(a, b)| a == b).count();
        assert!(same < population.len(), "every installation kept its place across releases");
    }

    #[test]
    fn a_missing_percentage_means_fully_published() {
        // Every index written before staged rollouts existed has no such
        // field, and must keep working.
        assert_eq!(declared_percentage(None), 100);
    }

    #[test]
    fn a_nonsense_percentage_from_the_server_cannot_break_updating() {
        // The index is attacker-controlled. A value above 100 means the same
        // thing it would if honest.
        assert_eq!(declared_percentage(Some(200)), 100);
        assert_eq!(declared_percentage(Some(100)), 100);
        assert_eq!(declared_percentage(Some(0)), 0);
    }

    #[test]
    fn two_identifiers_are_not_the_same() {
        let a = new_rollout_id().unwrap();
        let b = new_rollout_id().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32, "128 bits, hex encoded");
    }
}
