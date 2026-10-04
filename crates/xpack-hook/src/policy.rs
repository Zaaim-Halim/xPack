//! What a hook may touch and run.
//!
//! A hook is the publisher's signed code and is trusted as such: this is not
//! a sandbox, and a program a hook may run can do whatever the account running
//! it can. What this enforces is narrower and still worth having: a hook
//! reaches only the places and programs its package declared, never xPack's
//! own files, and in an installation for one user never anything that would
//! ask for administrator rights. A mistake stays inside those lines, and the
//! lines are what a reviewer reads.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use xpack_core::hooks::{Scope, asks_for_administrator};
use xpack_core::paths::InstallPaths;

use crate::request::Request;

/// Why a hook was refused something, in words for its error.
pub(crate) type Refusal = String;

/// The places and programs a hook may use, resolved for one run.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    scope: Scope,
    /// The installation. Everything in it is xPack's except what follows.
    installation: PathBuf,
    /// Readable inside the installation: every version's files.
    versions: PathBuf,
    /// Readable and writable, wherever they are: the data directory and the
    /// run's temporary directory.
    own: Vec<PathBuf>,
    /// Readable and writable, outside the installation only: the places the
    /// package declared for this scope.
    declared: Vec<PathBuf>,
    /// Programs a hook may run, by file name or absolute path.
    programs: Vec<String>,
}

impl Policy {
    /// The policy for `request`: its scope's declared permissions, placeholders
    /// replaced and every place resolved.
    pub(crate) fn of(request: &Request) -> Result<Self, Refusal> {
        let installation = resolve(&request.application_dir)?;
        let versions =
            resolve(&InstallPaths::from_application_dir(&request.application_dir).versions_dir())?;
        let own = vec![resolve(&request.data_dir)?, resolve(&request.temp_dir)?];
        let home = request.home.as_deref().map(resolve).transpose()?;
        let mut declared = Vec::new();
        for place in &request.permissions.write {
            let resolved = resolve(&resolve_placeholder(place, request)?)?;
            // Checked again here, after links are followed: a declaration
            // that was a place in the user's home when the package was made
            // can be a link out of it on this machine.
            if request.scope == Scope::User
                && !home.as_ref().is_some_and(|home| resolved.starts_with(home))
                && !resolved.starts_with(&own[1])
            {
                return Err(format!("{place} is {}, outside this user's home", resolved.display()));
            }
            declared.push(resolved);
        }
        Ok(Self {
            scope: request.scope,
            installation,
            versions,
            own,
            declared,
            programs: request.permissions.exec.clone(),
        })
    }

    /// Where `path` is, if it may be read.
    pub(crate) fn may_read(&self, path: &Path) -> Result<PathBuf, Refusal> {
        let resolved = resolve(path)?;
        let allowed = if resolved.starts_with(&self.installation) {
            resolved.starts_with(&self.versions) || self.is_own(&resolved)
        } else {
            self.is_own(&resolved) || self.is_declared(&resolved)
        };
        if allowed { Ok(resolved) } else { Err(self.refusal(path, &resolved, "read")) }
    }

    /// Where `path` is, if it may be created, changed or removed.
    ///
    /// Strictly inside an allowed place, never the place itself: a hook
    /// allowed `{home}/.config` may remove what is in it, not the directory
    /// a user's other programs rely on.
    pub(crate) fn may_write(&self, path: &Path) -> Result<PathBuf, Refusal> {
        let resolved = resolve(path)?;
        if self.writable(&resolved) && !self.is_a_place(&resolved) {
            Ok(resolved)
        } else {
            Err(self.refusal(path, &resolved, "write"))
        }
    }

    /// Where the entry `path` names is, if it may be removed or renamed.
    ///
    /// Everything up to the last component is resolved; the last is not
    /// followed. Removing or renaming acts on the name itself, so a link is
    /// judged as the link it is: removing one removes the link, never what it
    /// points to, wherever that is.
    pub(crate) fn may_change_entry(&self, path: &Path) -> Result<PathBuf, Refusal> {
        let lexical = lexical(path)?;
        let (Some(parent), Some(name)) = (lexical.parent(), lexical.file_name()) else {
            return Err(format!("{} names no entry", path.display()));
        };
        let entry = resolve(parent)?.join(name);
        if self.writable(&entry) && !self.is_a_place(&entry) {
            Ok(entry)
        } else {
            Err(self.refusal(path, &entry, "write"))
        }
    }

    /// Where `path` is, if a directory may be made there: as for writing, and
    /// an allowed place itself, which may not exist yet.
    pub(crate) fn may_make_dir(&self, path: &Path) -> Result<PathBuf, Refusal> {
        let resolved = resolve(path)?;
        if self.writable(&resolved) {
            Ok(resolved)
        } else {
            Err(self.refusal(path, &resolved, "write"))
        }
    }

    fn writable(&self, resolved: &Path) -> bool {
        // Inside the installation only its data and the run's temporary
        // directory are a hook's, whatever the package declared: a
        // declaration such as `{home}` contains a per-user installation, and
        // must not reach its versions, its state or the runtime programs.
        if resolved.starts_with(&self.installation) {
            self.is_own(resolved)
        } else {
            self.is_own(resolved) || self.is_declared(resolved)
        }
    }

    fn is_own(&self, resolved: &Path) -> bool {
        self.own.iter().any(|place| resolved.starts_with(place))
    }

    fn is_declared(&self, resolved: &Path) -> bool {
        self.declared.iter().any(|place| resolved.starts_with(place))
    }

    fn is_a_place(&self, resolved: &Path) -> bool {
        self.own.iter().chain(&self.declared).any(|place| resolved == place)
    }

    fn refusal(&self, asked: &Path, resolved: &Path, what: &str) -> Refusal {
        if resolved.starts_with(&self.installation) && !self.is_own(resolved) {
            let readable = what == "read" && resolved.starts_with(&self.versions);
            if !readable {
                return format!("{} belongs to xPack; no hook may {what} it", asked.display());
            }
        }
        if self.is_a_place(resolved) {
            return format!("{} is a place this hook may write in, not replace", asked.display());
        }
        format!("{} is not a place this hook may {what}", asked.display())
    }

    /// The file `program` names, if it may be run.
    ///
    /// A bare name is looked for along this process's own `PATH` here, before
    /// anything else can change it: the child's environment, which a hook
    /// sets, never chooses which program runs.
    pub(crate) fn program(&self, program: &str) -> Result<PathBuf, Refusal> {
        self.may_name(program)?;
        let path_like = program.contains(['/', '\\']);
        let file = if path_like {
            PathBuf::from(program)
        } else {
            find_on_path(program).ok_or_else(|| format!("{program} is not installed here"))?
        };
        // Windows starts a batch file through `cmd.exe`, which parses its
        // arguments as a command line: a shell, which a hook never gets.
        let extension = file.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if matches!(extension.as_deref(), Some("bat" | "cmd")) {
            return Err(format!("{program} is a batch file, which runs through a shell"));
        }
        // A link to an elevation program, under another name, is one too.
        if self.scope == Scope::User
            && let Ok(target) = std::fs::canonicalize(&file)
            && target.file_name().and_then(|n| n.to_str()).is_some_and(asks_for_administrator)
        {
            return Err(elevation(program));
        }
        Ok(file)
    }
}

/// `path` absolute, with `.` and `..` taken out as written.
fn lexical(path: &Path) -> Result<PathBuf, Refusal> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            other => lexical.push(other.as_os_str()),
        }
    }
    Ok(lexical)
}

impl Policy {
    /// Whether `program`, as a hook names it, may be run at all: declared,
    /// never an elevation program for one user, never a batch file. All a
    /// test that runs nothing needs: where the program is, and whether this
    /// machine has it, are the user's machine's business.
    pub(crate) fn may_name(&self, program: &str) -> Result<(), Refusal> {
        if self.scope == Scope::User && asks_for_administrator(program) {
            return Err(elevation(program));
        }
        let path_like = program.contains(['/', '\\']);
        let declared = self.programs.iter().any(|allowed| {
            if allowed.contains(['/', '\\']) {
                allowed == program
            } else {
                !path_like && same_program_name(allowed, program)
            }
        });
        if !declared {
            return Err(format!("{program} is not a program this package lets its hooks run"));
        }
        let extension = Path::new(program).extension().and_then(|e| e.to_str());
        if extension.is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"))
        {
            return Err(format!("{program} is a batch file, which runs through a shell"));
        }
        Ok(())
    }
}

fn elevation(program: &str) -> Refusal {
    format!("{program} asks for administrator rights, which a hook for one user never gets")
}

/// Whether a declared program name and a requested one are the same program:
/// exactly on Unix; on Windows without regard to case, and with or without
/// `.exe`, as Windows itself starts them.
fn same_program_name(declared: &str, requested: &str) -> bool {
    if cfg!(windows) {
        let bare = |name: &str| {
            let lower = name.to_ascii_lowercase();
            lower.strip_suffix(".exe").map_or(lower.clone(), str::to_string)
        };
        bare(declared) == bare(requested)
    } else {
        declared == requested
    }
}

/// The first file named `program` along this process's `PATH`, trying the
/// executable extensions on Windows.
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut names = vec![OsString::from(program)];
    if cfg!(windows) && Path::new(program).extension().is_none() {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for extension in extensions.split(';').filter(|e| !e.is_empty()) {
            names.push(OsString::from(format!("{program}{extension}")));
        }
    }
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// A declared place with its placeholder replaced by the directory it names.
fn resolve_placeholder(place: &str, request: &Request) -> Result<PathBuf, Refusal> {
    let pairs: [(&str, Option<&Path>); 3] = [
        ("{home}", request.home.as_deref()),
        ("{tempDir}", Some(request.temp_dir.as_path())),
        ("{programData}", request.program_data.as_deref()),
    ];
    for (placeholder, dir) in pairs {
        if let Some(rest) = place.strip_prefix(placeholder) {
            let dir = dir.ok_or_else(|| {
                format!("{place} names {placeholder}, which this installation does not have")
            })?;
            let rest = rest.trim_start_matches(['/', '\\']);
            return Ok(if rest.is_empty() { dir.to_path_buf() } else { dir.join(rest) });
        }
    }
    if request.scope == Scope::User {
        return Err(format!("{place} is not under {{home}} or {{tempDir}}"));
    }
    Ok(PathBuf::from(place))
}

/// Where `path` really is: absolute, without `.` or `..`, and with every link
/// along the way followed, so neither can lead a check astray.
///
/// The part that exists is resolved by the operating system; the rest, not
/// yet created, is appended as written. A link pointing nowhere, anywhere
/// along the path, is refused: what lies behind it is somewhere unknown, and
/// creating a file through it would put the file there.
pub(crate) fn resolve(path: &Path) -> Result<PathBuf, Refusal> {
    let lexical = lexical(path)?;
    let mut existing = lexical.clone();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            let mut resolved = real;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
        if std::fs::symlink_metadata(&existing).is_ok() {
            return Err(format!("{} leads through a link to nothing", path.display()));
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return Ok(lexical),
        }
    }
}
