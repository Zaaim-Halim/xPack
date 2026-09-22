//! The publisher's settings for the wizard.
//!
//! Written by the publisher, checked when the installer is built, and carried
//! inside it. Every field has a default, and a default always gives what the
//! installer without a window does: the same folder, the same desktop entry,
//! nothing started afterwards. A publisher who writes nothing gets the
//! recommended wizard over exactly that behaviour, and one who writes a single
//! field changes only that field.
//!
//! Branding is not here. Name, publisher, description and icon come from the
//! signed package manifest and nowhere else, so the window, the installer file
//! and the installed application can never disagree about who made it.
//!
//! None of this is signed. It sits beside the package, not inside it, so it
//! may only shape how the wizard *looks*. The one setting that touches the
//! installation, [`UiPlan::shortcut_default`], can only start a box unticked,
//! which takes away an entry the signed package asked for and can never add
//! one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xpack_core::{Error, Result};

use super::page::{Page, PageSet};
use super::text::{self, TextOverride};

/// The wizard settings a publisher supplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiPlan {
    /// Format of this document.
    #[serde(default = "UiPlan::current_version")]
    pub format_version: u32,

    /// Which pages appear. Left out, the recommended sequence.
    ///
    /// The order written here is ignored: pages always appear in the one order
    /// that makes sense.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<Page>>,

    /// The licence to show, as plain text. Left out, there is no licence page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,

    /// Replacements for the few lines a publisher may reword.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub text: BTreeMap<TextOverride, String>,

    /// Whether the shortcut box starts ticked, where it is offered at all.
    ///
    /// Ticked by default, because a publisher whose package asks for a desktop
    /// entry wants one, and most users do too.
    #[serde(default = "enabled")]
    pub shortcut_default: bool,

    /// Whether the last page offers to start the application.
    ///
    /// Off by default, because the installer without a window never starts
    /// anything, and leaving a setting out gives exactly what that installer
    /// does.
    #[serde(default)]
    pub launch_on_finish: bool,
}

impl Default for UiPlan {
    fn default() -> Self {
        Self {
            format_version: Self::CURRENT_VERSION,
            pages: None,
            license: None,
            text: BTreeMap::new(),
            shortcut_default: true,
            launch_on_finish: false,
        }
    }
}

impl UiPlan {
    /// Format written by this build.
    pub const CURRENT_VERSION: u32 = 1;

    fn current_version() -> u32 {
        Self::CURRENT_VERSION
    }

    /// Checks everything that can be checked without reading a file.
    ///
    /// The licence is a path whose contents are checked by whoever can read
    /// it; this covers the rest, so that a mistake is reported to
    /// the publisher building the installer rather than to a user running it.
    pub fn validate(&self) -> Result<()> {
        if self.format_version > Self::CURRENT_VERSION {
            return Err(invalid(format!(
                "is format {} but this build understands {}",
                self.format_version,
                Self::CURRENT_VERSION
            )));
        }
        if self.license.as_deref().is_some_and(|path| path.trim().is_empty()) {
            return Err(invalid("license must not be empty".to_string()));
        }
        for (key, value) in &self.text {
            text::validate_override(*key, value)?;
        }
        Ok(())
    }

    /// The pages this plan shows.
    ///
    /// Only the licence page can be empty: every other page is filled from the
    /// package and the machine. A licence page with no licence is skipped.
    pub fn page_set(&self) -> PageSet {
        self.page_set_given(self.license.is_some())
    }

    /// The pages shown, given whether a licence text is actually present.
    ///
    /// The one rule for which pages appear, used both when an installer is
    /// built and when its wizard opens.
    pub fn page_set_given(&self, has_licence: bool) -> PageSet {
        let has_content = |page: Page| page != Page::Licence || has_licence;
        match &self.pages {
            Some(pages) => PageSet::new(pages.iter().copied(), has_content),
            None => PageSet::recommended(has_content),
        }
    }
}

fn enabled() -> bool {
    true
}

fn invalid(reason: String) -> Error {
    Error::invalid("installer settings", reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> std::result::Result<UiPlan, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn an_empty_document_is_the_recommended_wizard() {
        let plan = parse("{}").expect("an empty document");
        assert_eq!(plan, UiPlan::default());
        plan.validate().expect("the defaults are valid");
        assert!(!plan.page_set().contains(Page::Licence), "no licence, no licence page");
    }

    #[test]
    fn leaving_a_setting_out_gives_what_the_installer_without_a_window_does() {
        let plan = parse("{}").expect("an empty document");
        // The package's own request for a desktop entry stands.
        assert!(plan.shortcut_default);
        // Nothing is started afterwards.
        assert!(!plan.launch_on_finish);
    }

    #[test]
    fn a_license_alone_adds_the_license_page() {
        let plan = parse(r#"{"license": "LICENSE.txt"}"#).expect("a document");
        assert!(plan.page_set().contains(Page::Licence));
    }

    #[test]
    fn a_license_page_without_a_license_is_skipped() {
        let plan =
            parse(r#"{"pages": ["welcome", "license", "install", "finish"]}"#).expect("parses");
        plan.validate().expect("valid");
        assert!(!plan.page_set().contains(Page::Licence));
    }

    #[test]
    fn a_full_document_parses() {
        let plan = parse(
            r#"{
                "formatVersion": 1,
                "pages": ["welcome", "license", "location", "ready", "install", "finish"],
                "license": "LICENSE.txt",
                "text": { "welcome": "Hello {name}.", "finish": "Done." },
                "shortcutDefault": false,
                "launchOnFinish": true
            }"#,
        )
        .expect("a document");
        plan.validate().expect("valid");
        assert!(!plan.shortcut_default);
        assert!(plan.launch_on_finish);
        assert_eq!(plan.text.len(), 2);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        // A misspelt setting silently falling back to its default is exactly
        // the mistake nobody notices until a user does.
        assert!(parse(r#"{"launchOnFinnish": true}"#).is_err());
    }

    #[test]
    fn an_unknown_page_is_refused_with_the_valid_names() {
        let error = parse(r#"{"pages": ["welcome", "extras"]}"#).expect_err("an unknown page");
        let message = error.to_string();
        assert!(message.contains("extras") && message.contains("welcome"), "{message}");
    }

    #[test]
    fn an_unknown_text_key_is_refused() {
        assert!(parse(r#"{"text": {"licence.title": "Terms"}}"#).is_err());
    }

    #[test]
    fn a_newer_format_is_refused() {
        let plan = parse(r#"{"formatVersion": 2}"#).expect("parses");
        assert!(plan.validate().is_err());
    }

    #[test]
    fn an_empty_path_is_refused() {
        assert!(parse(r#"{"license": " "}"#).expect("parses").validate().is_err());
    }

    #[test]
    fn branding_is_not_accepted_here() {
        // Branding has one source, the signed manifest. A second place to set
        // it would let the window and the package disagree.
        for field in ["logo", "icon", "name", "publisher"] {
            let json = format!(r#"{{"{field}": "x"}}"#);
            assert!(parse(&json).is_err(), "{field} was accepted");
        }
    }

    #[test]
    fn a_plan_survives_a_round_trip() {
        let plan = UiPlan { license: Some("L.txt".into()), ..UiPlan::default() };
        let json = serde_json::to_string(&plan).expect("serialises");
        assert_eq!(parse(&json).expect("parses back"), plan);
    }
}
