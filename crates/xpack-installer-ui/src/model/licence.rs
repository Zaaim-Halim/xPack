//! A licence laid out for a text box that wraps on its own.
//!
//! Licence files are usually hard-wrapped near 76 columns and indented for a
//! fixed-width font. In the wizard's proportional font and narrower box, each
//! of those lines wraps again part-way along, and the text reads as a ragged
//! column of half-lines. Rejoining the lines of each paragraph lets the box
//! wrap them to its own width.
//!
//! Only the line breaks and the indentation change. Every word is kept, in
//! order: a person agrees to exactly the text the publisher shipped.

/// Longest line, in characters, that still counts as hard-wrapped. A text
/// with longer lines was written for a box that wraps, and is left alone.
const HARD_WRAP_LIMIT: usize = 100;

/// `text` with the lines of each paragraph joined.
///
/// A line continues the one before it when that one ran close to the wrap
/// width, so short lines keep their breaks: a title block, an address, the
/// last line of a paragraph. A line that opens a list item always starts
/// afresh. Blank lines separate paragraphs, one blank line however many
/// there were.
#[must_use]
pub fn for_display(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let width = text.lines().map(|line| line.trim_end().chars().count()).max().unwrap_or(0);
    if width == 0 || width > HARD_WRAP_LIMIT {
        return text;
    }
    // Most wrapped lines stop within a word or two of the width.
    let full = width * 3 / 5;

    let mut out = String::with_capacity(text.len());
    let mut previous: Option<&str> = None;
    let mut blank_pending = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_pending = previous.is_some();
            continue;
        }
        match previous {
            None => {}
            Some(_) if blank_pending => out.push_str("\n\n"),
            Some(before) if before.chars().count() >= full && !opens_an_item(trimmed) => {
                out.push(' ');
            }
            Some(_) => out.push('\n'),
        }
        out.push_str(trimmed);
        previous = Some(trimmed);
        blank_pending = false;
    }
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Whether a line begins a list item: `1.`, `2)`, `(a)`, `(iv)`, `a.`, or a
/// bullet.
fn opens_an_item(line: &str) -> bool {
    let Some((marker, _)) = line.split_once(char::is_whitespace) else {
        return false;
    };
    if matches!(marker, "-" | "*" | "•") {
        return true;
    }
    if let Some(inner) = marker.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        return (1..=4).contains(&inner.len()) && inner.chars().all(|c| c.is_ascii_alphanumeric());
    }
    let Some(body) = marker.strip_suffix('.').or_else(|| marker.strip_suffix(')')) else {
        return false;
    };
    let digits = (1..=3).contains(&body.len()) && body.chars().all(|c| c.is_ascii_digit());
    let letter = body.len() == 1 && body.chars().all(|c| c.is_ascii_lowercase());
    digits || letter
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    const APACHE_EXCERPT: &str = "                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      \"License\" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      \"Licensor\" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and
";

    #[test]
    fn a_wrapped_paragraph_becomes_one_line() {
        let shown = for_display(APACHE_EXCERPT);
        assert!(
            shown.contains(
                "\"License\" shall mean the terms and conditions for use, reproduction, \
                 and distribution as defined by Sections 1 through 9 of this document.\n"
            ),
            "{shown}"
        );
        assert!(
            shown.contains("provided that You meet the following conditions:\n\n(a) You must give"),
            "{shown}"
        );
    }

    #[test]
    fn short_lines_keep_their_breaks() {
        let shown = for_display(APACHE_EXCERPT);
        assert!(
            shown.starts_with(
                "Apache License\nVersion 2.0, January 2004\nhttp://www.apache.org/licenses/\n\n"
            ),
            "{shown}"
        );
    }

    #[test]
    fn every_word_is_kept_in_order() {
        let licence = include_str!("../../../../LICENSE");
        assert_eq!(words(&for_display(licence)), words(licence));
        assert_eq!(words(&for_display(APACHE_EXCERPT)), words(APACHE_EXCERPT));
    }

    #[test]
    fn the_whole_apache_licence_is_one_line_per_paragraph() {
        // Every paragraph stands alone between blank lines; only the title
        // block keeps lines together, because each of them is short.
        let shown = for_display(include_str!("../../../../LICENSE"));
        let lines: Vec<&str> = shown.lines().collect();
        let joined_to_the_next: Vec<&str> = lines
            .windows(2)
            .filter(|pair| !pair[0].is_empty() && !pair[1].is_empty())
            .map(|pair| pair[0])
            .collect();
        assert_eq!(
            joined_to_the_next,
            ["Apache License", "Version 2.0, January 2004"],
            "a paragraph was left broken:\n{shown}"
        );
        assert!(!shown.contains("\n\n\n"), "{shown}");
    }

    #[test]
    fn a_list_item_after_a_full_line_starts_its_own_line() {
        let text = "The following conditions apply to anyone who uses this work at all:\n\
                    1. Keep this notice with every copy of the work that you make.\n\
                    2. Do not use the names of the authors to promote anything.\n";
        let shown = for_display(text);
        assert_eq!(shown.lines().count(), 3, "{shown}");
    }

    #[test]
    fn a_text_written_for_wrapping_is_left_alone() {
        let long = "word ".repeat(40);
        let text = format!("{long}\n{long}\n");
        assert_eq!(for_display(&text), text);
    }

    #[test]
    fn windows_line_endings_are_read_as_line_breaks() {
        let text = "A licence line that runs long enough to have been wrapped here,\r\n\
                    and its continuation.\r\n";
        assert_eq!(
            for_display(text),
            "A licence line that runs long enough to have been wrapped here, and its continuation.\n"
        );
    }

    #[test]
    fn an_empty_licence_stays_empty() {
        assert_eq!(for_display(""), "");
    }
}
