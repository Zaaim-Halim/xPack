//! Platforms with no dialog yet.
//!
//! Reporting that nothing was shown, rather than pretending the user declined,
//! is what lets a caller tell a prompt that failed from a person who said no.
//! The update itself is unaffected: it is already staged, and it will be used
//! at the next start exactly as it would on an installation that never asked
//! for a prompt.

use crate::{Answer, Prompt};

/// Shows nothing, and says so.
pub(crate) fn show(prompt: &Prompt) -> Answer {
    let _ = prompt;
    Answer::NotShown
}
