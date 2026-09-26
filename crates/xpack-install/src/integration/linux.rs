//! freedesktop desktop entries.
//!
//! Linux has no central application directory, so "install a shortcut" means
//! writing a `.desktop` file where the desktop environment looks for one:
//! `$XDG_DATA_HOME/applications`, which is `~/.local/share/applications` by
//! default. GNOME, KDE and XFCE all read it, and none of them needs root.
//!
//! The icon is referenced by absolute path rather than registered in the
//! `hicolor` theme. A themed icon has to be filed under the directory matching
//! its pixel size, and xPack does not decode images, so it cannot know the
//! size — a wrong guess puts the icon in a directory the theme will not read
//! at that resolution. The specification allows an absolute path precisely for
//! cases like this one, and every desktop environment honours it.

use std::path::PathBuf;

use super::{Entry, Outcome, Roots, remove_paths};

/// Writes the entry to the user's applications directory.
pub(super) fn install(entry: &Entry, roots: &Roots) -> Outcome {
    let path = entry_path(entry, roots);

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Outcome::Failed(format!("{}: {e}", parent.display()));
    }

    match xpack_core::atomic::write(&path, render(entry).as_bytes()) {
        Ok(()) => Outcome::Done(vec![path]),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Removes the entry.
pub(super) fn remove(entry: &Entry, roots: &Roots) -> Outcome {
    remove_paths(vec![entry_path(entry, roots)])
}

/// `$XDG_DATA_HOME/applications/<application-id>.desktop`.
///
/// Named after the application id rather than the display name: the id is
/// already validated as a safe filename, and the specification recommends a
/// reverse-DNS name so that two publishers cannot collide.
fn entry_path(entry: &Entry, roots: &Roots) -> PathBuf {
    roots.data.join("applications").join(format!("{}.desktop", entry.application_id))
}

/// Renders the desktop entry file.
///
/// Kept separate from the write so the format is testable on any host, which
/// matters because this is the one file here whose *contents* a user can see
/// and a desktop environment can reject.
pub(super) fn render(entry: &Entry) -> String {
    // `write!` into a String cannot fail — the only error a `fmt::Write`
    // implementation for String could return does not exist — so each result
    // is discarded rather than propagated through a function that has no way
    // to fail and no caller prepared for one.
    use std::fmt::Write as _;

    let mut out = String::from("[Desktop Entry]\n");
    out.push_str("Type=Application\n");
    out.push_str("Version=1.0\n");
    let _ = writeln!(out, "Name={}", escape(&entry.name));

    if let Some(description) = &entry.description {
        let _ = writeln!(out, "Comment={}", escape(description));
    }

    // Quoted because the installation root contains the application id and can
    // sit under a home directory with a space in it. An unquoted Exec with a
    // space is parsed as a command plus arguments.
    let _ = writeln!(out, "Exec=\"{}\" %U", escape(&entry.target.to_string_lossy()));

    if let Some(icon) = &entry.icon {
        let _ = writeln!(out, "Icon={}", escape(&icon.to_string_lossy()));
    }

    let _ = writeln!(out, "Terminal={}", entry.terminal);

    if !entry.categories.is_empty() {
        // The specification requires the trailing semicolon, and a desktop
        // environment that parses strictly drops the last category without it.
        let joined: Vec<String> = entry.categories.iter().map(|c| escape(c)).collect();
        let _ = writeln!(out, "Categories={};", joined.join(";"));
    }

    // Lets a desktop environment associate a running window with this entry,
    // which is what makes the correct icon appear in a dock or task switcher.
    let _ = writeln!(out, "StartupWMClass={}", escape(&entry.name));

    out
}

/// The launcher this user's `.desktop` entry for the application starts, if
/// there is one.
pub(super) fn recorded_target(application_id: &str, roots: &Roots) -> Option<PathBuf> {
    let path = roots.data.join("applications").join(format!("{application_id}.desktop"));
    parse_exec(&std::fs::read_to_string(path).ok()?)
}

/// Reads back the path the `Exec=` line holds, exactly as it was written.
fn parse_exec(entry: &str) -> Option<PathBuf> {
    let line = entry.lines().find_map(|line| line.strip_prefix("Exec=\""))?;
    let escaped = line.strip_suffix("\" %U")?;
    Some(PathBuf::from(unescape(escaped)))
}

/// Undoes [`escape`].
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Escapes the characters the desktop entry format gives meaning to.
///
/// A display name is free-form Unicode chosen by a publisher. A newline in one
/// would end the value and turn whatever followed into a new key, which is how
/// a crafted manifest would otherwise inject `Exec=`.
fn escape(value: &str) -> String {
    value.replace('\\', r"\\").replace('\n', r"\n").replace('\r', r"\r").replace('\t', r"\t")
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_path_an_entry_starts_reads_back_exactly() {
        let mut e = entry();
        for target in
            ["/home/me/.local/share/xpack/com.example.app/Example", "/home/My User/a\\b/Example"]
        {
            e.target = PathBuf::from(target);
            let text = render(&e);
            assert_eq!(parse_exec(&text), Some(PathBuf::from(target)), "{text}");
        }
    }
    use super::*;
    use std::path::Path;

    fn entry() -> Entry {
        Entry {
            application_id: "com.example.app".into(),
            name: "Example App".into(),
            description: Some("Does a thing".into()),
            publisher: Some("Example Ltd".into()),
            version: xpack_core::Version::parse("1.0.0").unwrap(),
            root: PathBuf::from("/home/u/.local/share/xpack/com.example.app"),
            target: PathBuf::from("/home/u/.local/share/xpack/com.example.app/xpack-launcher"),
            icon: Some(PathBuf::from("/home/u/.local/share/xpack/com.example.app/icon.png")),
            categories: vec!["Utility".into(), "Development".into()],
            terminal: false,
            uninstaller: None,
        }
    }

    #[test]
    fn renders_the_keys_a_desktop_environment_requires() {
        let text = render(&entry());
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Type=Application\n"));
        assert!(text.contains("Name=Example App\n"));
        assert!(text.contains("Terminal=false\n"));
    }

    #[test]
    fn quotes_the_executable_so_a_path_with_a_space_still_starts() {
        let mut e = entry();
        e.target = PathBuf::from("/home/My User/app/xpack-launcher");
        let text = render(&e);
        assert!(text.contains("Exec=\"/home/My User/app/xpack-launcher\" %U\n"), "{text}");
    }

    #[test]
    fn terminates_the_category_list_with_a_semicolon() {
        // A strict parser silently drops the final category without it.
        let text = render(&entry());
        assert!(text.contains("Categories=Utility;Development;\n"), "{text}");
    }

    #[test]
    fn a_newline_in_a_display_name_cannot_inject_a_key() {
        // The name comes from a manifest. Without escaping, this would end the
        // Name value and make the rest of the line a new Exec key.
        let mut e = entry();
        e.name = "Evil\nExec=/bin/sh -c rm".into();

        let text = render(&e);
        assert!(!text.contains("\nExec=/bin/sh"), "injection succeeded:\n{text}");
        assert!(text.contains(r"Name=Evil\nExec=/bin/sh -c rm"), "{text}");
    }

    #[test]
    fn omits_the_icon_key_entirely_when_there_is_no_icon() {
        // An `Icon=` with an empty value makes some environments show a broken
        // image rather than the default.
        let mut e = entry();
        e.icon = None;
        assert!(!render(&e).contains("Icon="));
    }

    #[test]
    fn a_terminal_application_is_marked_as_one() {
        let mut e = entry();
        e.terminal = true;
        assert!(render(&e).contains("Terminal=true\n"));
    }

    fn roots(base: &Path) -> Roots {
        Roots {
            data: base.join("data"),
            home: base.join("home"),
            desktop: Some(base.join("desktop")),
        }
    }

    #[test]
    fn the_file_is_named_after_the_application_id() {
        let path = entry_path(&entry(), &roots(Path::new("/tmp/x")));
        assert!(path.ends_with(Path::new("applications/com.example.app.desktop")), "{path:?}");
    }

    #[test]
    fn writing_then_removing_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let entry = entry();

        assert!(install(&entry, &roots).is_done());
        let path = entry_path(&entry, &roots);
        assert!(path.is_file(), "the entry was not written");
        assert!(std::fs::read_to_string(&path).unwrap().contains("Name=Example App"));

        assert!(remove(&entry, &roots).is_done());
        assert!(!path.exists(), "the entry survived removal");
    }
}
