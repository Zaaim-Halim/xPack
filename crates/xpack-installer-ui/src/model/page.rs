//! The pages, and the one order they ever appear in.

use serde::{Deserialize, Serialize};

/// One page of the wizard.
///
/// Declared in the order they are shown. That order is fixed: a publisher
/// chooses *which* pages appear, never their sequence, because a licence shown
/// after installing, or a location chosen after it was used, means nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Page {
    /// Who is being installed, and by whom.
    #[serde(rename = "welcome")]
    Welcome,
    /// The licence, which must be accepted to go on.
    #[serde(rename = "license")]
    Licence,
    /// Where it goes, and what is already there.
    #[serde(rename = "location")]
    Location,
    /// A summary before anything happens.
    #[serde(rename = "ready")]
    Ready,
    /// The installation running.
    #[serde(rename = "install")]
    Installing,
    /// How it went.
    #[serde(rename = "finish")]
    Finish,
}

impl Page {
    /// Every page, in display order.
    pub const ALL: [Self; 6] =
        [Self::Welcome, Self::Licence, Self::Location, Self::Ready, Self::Installing, Self::Finish];

    /// The name a publisher writes in the page list.
    pub fn name(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Licence => "license",
            Self::Location => "location",
            Self::Ready => "ready",
            Self::Installing => "install",
            Self::Finish => "finish",
        }
    }

    /// The line naming this page in a list of steps.
    pub fn step_key(self) -> super::text::Key {
        use super::text::Key;
        match self {
            Self::Welcome => Key::StepWelcome,
            Self::Licence => Key::StepLicence,
            Self::Location => Key::StepLocation,
            Self::Ready => Key::StepReady,
            Self::Installing => Key::StepInstalling,
            Self::Finish => Key::StepFinish,
        }
    }

    /// Whether the page runs before anything is installed.
    pub fn is_before_install(self) -> bool {
        self < Self::Installing
    }
}

/// The pages a particular wizard shows, always in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSet {
    pages: Vec<Page>,
}

impl PageSet {
    /// The recommended sequence: every page that has something to show.
    pub fn recommended(has_content: impl Fn(Page) -> bool) -> Self {
        Self::new(Page::ALL, has_content)
    }

    /// A publisher's selection, in display order, without the pages that have
    /// nothing to show.
    ///
    /// Nothing about a selection is an error. A page named twice is shown
    /// once. The installation and its result are always shown, because a
    /// wizard without them does nothing. A page with no content — a licence
    /// page with no licence — is skipped, exactly as if it had not been
    /// listed, which is what leaving something out means everywhere else.
    ///
    /// One page is added rather than skipped. If nothing is left before the
    /// installation, the summary page is shown: otherwise the installation
    /// would start the moment the window opened, before the person had agreed
    /// to anything.
    pub fn new(
        selected: impl IntoIterator<Item = Page>,
        has_content: impl Fn(Page) -> bool,
    ) -> Self {
        let mut pages: Vec<Page> = selected
            .into_iter()
            .chain([Page::Installing, Page::Finish])
            .filter(|page| has_content(*page))
            .collect();
        if !pages.iter().any(|page| page.is_before_install()) {
            pages.push(Page::Ready);
        }
        pages.sort_unstable();
        pages.dedup();
        Self { pages }
    }

    /// Whether the page is shown.
    pub fn contains(&self, page: Page) -> bool {
        self.pages.contains(&page)
    }

    /// The pages, in display order.
    pub fn iter(&self) -> impl Iterator<Item = Page> + '_ {
        self.pages.iter().copied()
    }

    /// The page the wizard opens on.
    pub fn first(&self) -> Page {
        self.pages[0]
    }

    /// The page after `page`, if any.
    pub fn after(&self, page: Page) -> Option<Page> {
        self.iter().find(|candidate| *candidate > page)
    }

    /// The page before `page`, if any.
    pub fn before(&self, page: Page) -> Option<Page> {
        self.iter().filter(|candidate| *candidate < page).last()
    }

    /// The last page before installing: the one whose primary button starts it.
    ///
    /// # Panics
    ///
    /// Never: [`Self::new`] always leaves a page before the installation.
    pub fn last_before_install(&self) -> Page {
        self.before(Page::Installing).expect("a page set always has a page before installing")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn everything(_: Page) -> bool {
        true
    }

    fn no_licence(page: Page) -> bool {
        page != Page::Licence
    }

    #[test]
    fn the_recommended_sequence_is_every_page_in_order() {
        assert_eq!(PageSet::recommended(everything).iter().collect::<Vec<_>>(), Page::ALL);
    }

    #[test]
    fn a_page_with_nothing_to_show_is_skipped() {
        let set = PageSet::recommended(no_licence);
        assert!(!set.contains(Page::Licence));
        assert_eq!(set.iter().count(), 5);

        // Listed explicitly, it is skipped all the same.
        let set = PageSet::new([Page::Welcome, Page::Licence, Page::Installing], no_licence);
        assert!(!set.contains(Page::Licence));
    }

    #[test]
    fn a_selection_is_shown_in_display_order_whatever_order_it_was_written_in() {
        let set =
            PageSet::new([Page::Finish, Page::Ready, Page::Installing, Page::Welcome], everything);
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            [Page::Welcome, Page::Ready, Page::Installing, Page::Finish]
        );
    }

    #[test]
    fn a_page_listed_twice_is_shown_once() {
        let set = PageSet::new([Page::Welcome, Page::Welcome], everything);
        assert_eq!(set.iter().filter(|page| *page == Page::Welcome).count(), 1);
    }

    #[test]
    fn the_install_and_finish_pages_are_always_shown() {
        let set = PageSet::new([Page::Welcome], everything);
        assert!(set.contains(Page::Installing));
        assert!(set.contains(Page::Finish));
    }

    #[test]
    fn a_wizard_that_would_install_the_moment_it_opened_gets_a_summary_first() {
        // Nothing listed before the installation, or only a page that was
        // skipped: the summary is what the person agrees to.
        for selected in [vec![], vec![Page::Licence]] {
            let set = PageSet::new(selected, no_licence);
            assert_eq!(set.first(), Page::Ready);
            assert_eq!(set.last_before_install(), Page::Ready);
        }
    }

    #[test]
    fn neighbours_skip_the_pages_that_are_not_shown() {
        let set = PageSet::new([Page::Welcome, Page::Ready], everything);
        assert_eq!(set.after(Page::Welcome), Some(Page::Ready));
        assert_eq!(set.before(Page::Ready), Some(Page::Welcome));
        assert_eq!(set.before(Page::Welcome), None);
        assert_eq!(set.after(Page::Finish), None);
        assert_eq!(set.last_before_install(), Page::Ready);
        assert_eq!(set.first(), Page::Welcome);
    }
}
