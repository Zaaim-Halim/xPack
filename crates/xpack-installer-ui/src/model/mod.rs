//! Everything the wizard decides, with no window in sight.
//!
//! A front-end asks [`Wizard`] which page to show, what each button looks like
//! and what every line of text says, then reports back what the person did.
//! It makes no decisions of its own, so every rule is tested here once rather
//! than trusted to two platforms' worth of drawing code.

pub mod page;
pub mod plan;
pub mod progress;
pub mod status;
pub mod text;
pub mod wizard;

pub use page::{Page, PageSet};
pub use plan::UiPlan;
pub use progress::{Counts, Progress};
pub use status::{InstallKind, Severity, Status};
pub use text::{Facts, Flavour, Key, TextOverride, Texts};
pub use wizard::{
    Buttons, CloseRequest, Conclusion, FinishView, Step, Visibility, Wizard, WizardSpec,
};
