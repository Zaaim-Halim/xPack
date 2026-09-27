//! The password a locked application's packages are sealed with, and opening
//! a sealed package for a command that reads one.
//!
//! The password never comes from a file xPack writes or a command-line value
//! other users could see in the process list: it is read from an environment
//! variable, or from the first line of standard input.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use xpack_core::{Error, Result};
use xpack_security::seal::{self, SealKey};
use zeroize::Zeroizing;

/// The variable the password is read from when nothing names another.
pub(crate) const DEFAULT_PASSWORD_ENV: &str = "XPACK_PASSWORD";

/// Where a command reads the password from.
#[derive(ClapArgs, Debug, Clone, Default)]
pub(crate) struct PasswordArgs {
    /// Read the password from this environment variable (default
    /// `XPACK_PASSWORD`, or what the configuration names).
    #[arg(long, value_name = "VAR", conflicts_with = "password_stdin")]
    pub(crate) password_env: Option<String>,

    /// Read the password from the first line of standard input.
    #[arg(long)]
    pub(crate) password_stdin: bool,
}

impl PasswordArgs {
    /// The password, from standard input or the variable named, `None` when
    /// neither holds one.
    pub(crate) fn read(&self, configured_env: Option<&str>) -> Result<Option<Zeroizing<String>>> {
        if self.password_stdin {
            // Read once: a command opening several sealed files asks for the
            // password once, and standard input has only the one line.
            static FROM_STDIN: std::sync::OnceLock<Option<Zeroizing<String>>> =
                std::sync::OnceLock::new();
            if let Some(read) = FROM_STDIN.get() {
                return Ok(read.clone());
            }
            let mut line = Zeroizing::new(String::new());
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| Error::invalid("password", format!("standard input: {e}")))?;
            let password = Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string());
            let password = (!password.is_empty()).then_some(password);
            return Ok(FROM_STDIN.get_or_init(|| password).clone());
        }
        let name = self.password_env.as_deref().or(configured_env).unwrap_or(DEFAULT_PASSWORD_ENV);
        Ok(std::env::var(name).ok().filter(|value| !value.is_empty()).map(Zeroizing::new))
    }

    /// The password, or an error saying where it was looked for.
    pub(crate) fn require(&self, configured_env: Option<&str>) -> Result<Zeroizing<String>> {
        self.read(configured_env)?.ok_or_else(|| {
            let name =
                self.password_env.as_deref().or(configured_env).unwrap_or(DEFAULT_PASSWORD_ENV);
            Error::invalid(
                "password",
                format!("none given: set {name}, or pass --password-stdin and type it"),
            )
        })
    }
}

/// A package ready to read: the file itself, or its opened copy when it was
/// sealed, with the key that opened it.
pub(crate) struct Opened {
    /// The unsealed package.
    pub(crate) path: PathBuf,
    /// The key it was opened with, when it was sealed.
    pub(crate) key: Option<SealKey>,
    /// Holds the opened copy for as long as this lives.
    _dir: Option<tempfile::TempDir>,
}

impl Opened {
    /// Whether the package was sealed.
    pub(crate) fn was_sealed(&self) -> bool {
        self.key.is_some()
    }
}

/// Opens `package` for reading when it is sealed, asking `password` for the
/// password only then.
pub(crate) fn open(package: &Path, password: &PasswordArgs) -> Result<Opened> {
    if !seal::is_sealed(package)? {
        return Ok(Opened { path: package.to_path_buf(), key: None, _dir: None });
    }
    let application = seal::application_of(package)?;
    let password = password.require(None).map_err(|_| {
        Error::invalid(
            "package",
            format!(
                "{} is sealed with a password; give it with {DEFAULT_PASSWORD_ENV}, \
                 --password-env or --password-stdin",
                package.display()
            ),
        )
    })?;
    let key = SealKey::derive(&password, &application)?;
    let dir = tempfile::tempdir().map_err(|e| Error::io(Path::new("a temporary directory"), e))?;
    let path = dir.path().join("opened.xpkg");
    seal::open(package, &path, &key)?;
    Ok(Opened { path, key: Some(key), _dir: Some(dir) })
}

/// Seals the package at `plain` into `output`, for `application_id`.
pub(crate) fn seal_to(
    plain: &Path,
    output: &Path,
    application_id: &str,
    key: &SealKey,
) -> Result<()> {
    seal::seal(plain, output, application_id, key)
}

/// The key a new password gives `application_id`, refusing one too short.
pub(crate) fn key_for(password: &str, application_id: &str) -> Result<SealKey> {
    seal::ensure_strong_enough(password)?;
    SealKey::derive(password, application_id)
}
