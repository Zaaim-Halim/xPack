//! When the next update check happens.
//!
//! Every installation of an application checks on the same interval. Machines
//! imaged together, or a class of laptops opened at nine o'clock, would then
//! ask the publisher's server in one burst on every check, for ever. Each
//! check is therefore given a random extra wait, drawn afresh on every machine
//! each time, which spreads a fleet out after its first check.
//!
//! The rule the wait is applied by lives with the installation's state; this
//! draws it.

use xpack_core::state::check_delay_limit;

/// A random extra wait before the next check, from nothing up to
/// [`check_delay_limit`] of `interval_seconds`, in seconds.
///
/// # Errors
///
/// When the system has no source of randomness. A caller that cannot draw
/// should check without a delay rather than not at all: a fleet in step is a
/// load problem, never a correctness one.
pub fn random_check_delay(interval_seconds: u64) -> xpack_core::Result<u64> {
    let limit = check_delay_limit(interval_seconds);
    if limit == 0 {
        return Ok(0);
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| {
        xpack_core::Error::Unsupported(format!("no source of randomness available: {e}"))
    })?;
    // The bias of a modulo over 64 bits is far below anything a check
    // schedule could show.
    Ok(u64::from_le_bytes(bytes) % (limit + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xpack_core::InstallState;

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn a_delay_is_never_more_than_a_quarter_of_the_interval() {
        for _ in 0..10_000 {
            assert!(random_check_delay(DAY).unwrap() <= DAY / 4);
        }
    }

    #[test]
    fn an_interval_too_short_to_divide_gets_no_delay() {
        assert_eq!(random_check_delay(0).unwrap(), 0);
        assert_eq!(random_check_delay(3).unwrap(), 0);
    }

    #[test]
    fn a_fleet_that_checked_together_does_not_check_together_again() {
        // A thousand machines imaged together and started together: the same
        // state, the same first check, at the same second.
        let now = 1_000_000;
        let mut next: Vec<u64> = Vec::new();
        for _ in 0..1000 {
            let mut state = InstallState::new("com.example.app");
            state.record_update_check(now, random_check_delay(DAY).unwrap());
            // The first second at which this machine asks again.
            let due = (now..=now + DAY + DAY / 4)
                .step_by(60)
                .find(|&t| state.update_check_is_due(t, DAY))
                .unwrap();
            next.push(due);
        }

        // Nobody asks before the interval is up, and everybody by its end
        // plus a quarter.
        assert!(next.iter().all(|&t| t >= now + DAY && t <= now + DAY + DAY / 4));

        // Spread across the window rather than bunched: no single minute
        // holds more than a small share of the fleet. Evenly spread, a
        // six-hour window of minutes holds under one machine each.
        let mut per_minute = std::collections::BTreeMap::new();
        for t in &next {
            *per_minute.entry(t / 60).or_insert(0u32) += 1;
        }
        let busiest = per_minute.values().copied().max().unwrap();
        assert!(busiest <= 10, "{busiest} of 1000 machines asked in the same minute");
        assert!(per_minute.len() > 200, "only {} distinct minutes", per_minute.len());
    }
}
