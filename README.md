# xPack

**Package, install, update and roll back desktop and command-line applications
on Windows, macOS and Linux — whatever language they are written in.**

xPack turns a directory of files into a signed package, installs it for the
current user without administrator rights, keeps it up to date in the
background, and puts the previous version back if a new one fails to start.
It ships as a handful of small native binaries written in Rust; nothing of
xPack becomes part of your application.

> **Status: pre-release (0.8.0).** The command line, installing, updating,
> rollback and hooks are implemented, and the test suite runs on Windows, macOS and Linux
> in CI. The installation
> wizard is tested on macOS; on Windows it builds but has not yet been run on a
> real machine. Expect breaking changes before 1.0.

---

## Why xPack

- **Any runtime.** A Java app with a bundled JDK, a Python app with its
  interpreter, a native binary, a shell script: xPack starts an executable with
  arguments and an environment, and never asks what it is.
- **Signed end to end.** Every package is signed with Ed25519. An installation
  pins the publisher's key on first install and refuses anything else after.
- **Safe updates.** Updates install beside the running version and switch over
  atomically. A new version must prove it starts; if it does not, the previous
  one comes back on its own.
- **No administrator rights.** Everything is installed per user. There is no
  UAC prompt, no `sudo`, no system directory.
- **Staged rollout and delta updates.** Offer a release to 5% of users first,
  and ship only what changed between versions.
- **A real installer.** One file a user downloads and double-clicks, with a
  native installation wizard, or runs with `--silent` from a script.
- **Hooks.** Scripts the package runs once at moments of an installation's
  life, to set up a service, migrate data or clean up, limited to what they
  declare and tested before they can ship.

## How it works

```text
 your build ──► payload/ + xpack.json
                      │
                xpack pack ──► MyApp-1.0.0-<platform>.xpkg   (signed)
                      │
     ┌────────────────┼──────────────────────┐
     ▼                ▼                      ▼
 xpack installer   xpack index           xpack install
 (setup file for   (update index for     (install directly,
  new users)        existing users)       e.g. in CI)

 On the user's machine:
   launcher ── starts the active version, confirms it is healthy
   updater  ── checks the index, downloads, verifies, installs beside it
   uninstaller, and an optional "update ready" dialog
   xpack-hook ── runs the package's hooks, when it has any
```

## Supported platforms

| Platform | Architectures |
| --- | --- |
| Windows | x64 |
| macOS | Apple Silicon (arm64), Intel (x64) |
| Linux | x64, arm64 (static musl binaries, any distribution) |

## Installing xPack

**From a release** on macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/Zaaim-Halim/xPack/main/install.sh | sh
```

The script installs into `~/.local/xpack`. Set `XPACK_VERSION` to pick a
version and `XPACK_INSTALL_DIR` to install elsewhere.

On Windows, or to install by hand, download the archive for your platform from
the [releases page](https://github.com/Zaaim-Halim/xPack/releases), check it
against `SHA256SUMS`, unpack it and put the directory on your `PATH`. The
binaries are not code signed yet, so a browser download is quarantined on
macOS and flagged by SmartScreen on Windows.

**From source** (needs Rust 1.89 or newer):

```sh
git clone https://github.com/Zaaim-Halim/xPack.git
cd xPack
cargo build --release --workspace
```

The binaries are in `target/release`.

Either way, keep the binaries together in one directory: the `xpack` command
finds the launcher, updater and installer stubs beside itself.

**With an installer**, from a release that has one: `Install-xPack-…` for your
platform, next to the archives. It puts `xpack` and `cargo-xpack` on your
`PATH`, and keeps xPack up to date the way it keeps any application up to
date. xPack's packages and installers are signed with the key in
[`keys/xpack-release.pub.json`](keys/xpack-release.pub.json), fingerprint
**`ef16-89d5-607a-b505`**; an installation of xPack accepts updates signed by
nothing else.

## Quick start

Package a tiny application, install it, run it, and build an installer for it.

**1. Create a signing key** — once, and keep the private key out of your
project:

```sh
xpack keygen --out ~/keys/signing.json
```

This writes `signing.json` (private) and `signing.pub.json` (public).

**2. Describe the application.** A payload directory holds your files; an
`xpack.json` beside it says how to start them:

```sh
mkdir -p hello/payload/bin && cd hello
printf '#!/bin/sh\necho "Hello from xPack"\n' > payload/bin/hello
chmod +x payload/bin/hello
```

```json
{
  "application": {
    "id": "com.example.hello",
    "name": "Hello",
    "version": "1.0.0",
    "publisher": "Example Ltd"
  },
  "launch": { "executable": "bin/hello" }
}
```

**3. Package, install and run it:**

```sh
xpack pack payload --key ~/keys/signing.json         # Hello-1.0.0-<platform>.xpkg
xpack verify Hello-1.0.0-*.xpkg --key ~/keys/signing.pub.json
xpack install Hello-1.0.0-*.xpkg --trust ~/keys/signing.pub.json
xpack run com.example.hello                           # Hello from xPack
```

**4. Build the file you give to users:**

```sh
xpack installer Hello-1.0.0-*.xpkg --out-dir dist
```

On macOS this is `Install Hello.app`, on Windows `Hello-1.0.0-windows-x64-Setup.exe`,
on Linux `Hello-1.0.0-linux-x64-installer`. It needs nothing of xPack on the
user's machine.

## Configuration: `xpack.json`

```json
{
  "application": {
    "id": "com.example.myapp",
    "name": "My Application",
    "version": "1.2.0",
    "description": "Does a thing",
    "publisher": "Example Ltd"
  },
  "launch": {
    "executable": "runtime/bin/java",
    "arguments": ["-jar", "application/app.jar"],
    "environment": { "JAVA_TOOL_OPTIONS": "-Xmx512m" }
  },
  "update": {
    "channel": "stable",
    "url": "https://updates.example.com/myapp/macos-arm64",
    "mandatory": false
  },
  "health": { "startupTimeoutSeconds": 15, "requireStartupReport": false },
  "desktop": {
    "shortcut": true,
    "icon": "resources/icon.png",
    "categories": ["Utility"],
    "terminal": false
  }
}
```

| Section | What it does |
| --- | --- |
| `application` | Identity. `id` is a stable reverse-DNS name; `name`, `publisher` and the icon are what users see everywhere. |
| `launch` | The executable to start, relative to the payload (a bundled runtime) or a bare name found on `PATH` (a system one), with its arguments and environment. See [command-line tools](#command-line-tools) for where it starts. |
| `update` | Where this platform's update index lives, and the channel to follow. `mandatory` stops older versions from starting. |
| `health` | How long a new version has to prove it starts. With `requireStartupReport`, the application must write the file named in `XPACK_HEALTH_FILE`. |
| `desktop` | A Start Menu entry, a `~/Applications` bundle or a `.desktop` file, and the icon: `.ico` or `.png` on Windows, `.icns` on macOS, `.png` or `.svg` on Linux. |
| `instance` | `"single": true` keeps one copy running for each user. See [one running copy](#one-running-copy). |
| `hooks` | Scripts run once when the application is installed, updated, rolled back or uninstalled. See [hooks](#hooks). |

### Command-line tools

An application starts in its installed version directory, which is what a
program opened from a menu or Finder wants. **Set `keepWorkingDirectory` when
your application is also used from a command line**: a command-line tool, or
an application that takes file paths in a terminal (`myeditor notes.txt`,
`mytool build src`). It then starts in the directory the user ran it from, so
the paths they type mean what they meant.

Paths into your own package then need `{versionDir}`, which the launcher
replaces with the installed version directory:

```json
"launch": {
  "executable": "runtime/bin/java",
  "arguments": ["-cp", "{versionDir}/application/*", "com.example.Main"],
  "keepWorkingDirectory": true
}
```

The executable needs nothing: it is always found inside the package.

**To let users type it by name**, give it a command:

```json
"command": { "name": "mytool" }
```

The installer then offers "Add “mytool” to the command line", ticked unless
`"pathDefault": false` is set in the installer settings, and `--no-path`
declines it in a silent install. On macOS and Linux it becomes a small script
in `~/.local/bin`; the installer says so when that directory is not on the
user's `PATH`. On Windows the installation's own `bin` directory is added to
the user's `PATH`. A `mytool` that belongs to another program is never
replaced, and uninstalling removes the command. Names are lowercase letters,
digits, `.`, `_` and `-`.

**A companion tool** that ships in the same package gets a command of its own,
starting its own program:

```json
"commands": [{ "name": "mytool-helper", "executable": "bin/mytool-helper" }]
```

It is offered, declined and removed together with the main command, and runs
with the same environment and working-directory rules. A name another program
owns is left out; the others are still added.

`keepWorkingDirectory`, `{versionDir}` and `command` each make the package
format 2, which installations made with xPack 0.1.0 cannot install or update
to; `commands` makes it format 3, which installations made with 0.2.0 or earlier
cannot install or update to.

### One running copy

**To keep one copy of the application running for each user**:

```json
"instance": { "single": true }
```

A second start then starts nothing. It passes its arguments to the running
copy, brings that copy to the front where the platform allows, and exits:

- macOS brings it to the front itself when the application is opened again
  from Finder or the Dock, and a start from the command opens the bundle to do
  the same.
- On Windows the launcher brings the application's window to the front.
- Linux has no portable way to do it, so the application does it when it reads
  the request.

The running copy finds each request as a file in the directory named by
`XPACK_INSTANCE_INBOX`: `<something>.json`, holding `{"arguments": [...]}`.
Each file is complete when it appears. Read it, delete it, and treat it as the
user asking for your window with those arguments, a file opened from the
desktop, say. Requests left when the application closes are discarded at its
next start. An application that ignores the directory still runs once; only
the arguments are lost.

The copy is held by the launcher, not the application, so a launch program
that starts the real one and exits leaves no copy running as far as xPack can
tell. A copy that crashes never keeps the next start out. The application's
other `commands` are not limited.

`instance` makes the package format 4. Installations made with xPack 0.5.0 or
earlier cannot update to it, and an installer refuses to install it over one of
them, because their launcher is kept and could not start it. Uninstall first.

**An application that also answers on the command line** lists the arguments
that are commands:

```json
"instance": { "single": true, "alongside": ["--version", "--status", "export"] }
```

Handed over, `myapp --status` would print nothing and report success, because
the copy that would have answered it was never started. A start given any of
these arguments runs beside the running copy instead, with its own output and
exit code. Each entry is one argument as the user types it, and also matches
it with a value: `--export` matches `--export=out.csv`, but not
`--export-all`.

Such a start is not the running copy: a later start is never handed over to
it, and it is not told where requests arrive. It takes no part in updates: it
runs the active version, never activates a staged one, and checks for none.
When a mandatory release is waiting, it refuses until the application has been
started on it. It may run while the window is open, so both may use the same
files at once; the application has to allow for that.

`alongside` makes the package format 5, with the same rule: installations made
with xPack 0.6.x or earlier cannot update to it until they are uninstalled and
installed again.

The platform defaults to the machine you build on; `xpack pack --platform
windows-x64` builds for another.

## Shipping updates

Updates are static files on any web server or object store.

```sh
# Package the new version, then write the index clients read.
xpack pack payload --key ~/keys/signing.json
xpack index MyApp-1.3.0-*.xpkg --out-dir updates --key ~/keys/signing.pub.json
```

This writes `updates/<platform>/stable.json`. Upload the `updates` directory
**and copy each package into its platform directory beside the index**, so
that `<update.url>/stable.json` and the package sit side by side.

Or keep the packages somewhere else, such as attached to a GitHub Release,
and upload only the small index files: `--package-url` names each package by
its full address.

```sh
xpack index MyApp-1.3.0-*.xpkg --out-dir updates --key ~/keys/signing.pub.json \
  --package-url https://github.com/example/myapp/releases/download/v1.3.0
```

Installed applications check in the background and apply the update on the
next start. Each check waits a random extra while, up to a quarter of the
interval, so machines set up together do not all ask your server at the same
moment. You can also trigger it by hand:

```sh
xpack update com.example.myapp
xpack rollback com.example.myapp     # back to the previous healthy version
```

A release with [hooks](#hooks) is indexed only once they have passed
`xpack hooks test`.

**Staged rollout:** `--rollout 5` offers the release to 5% of installations;
re-run with a higher number to widen it, or `--rollout 0` to stop it.
**Delta updates:** `xpack delta` builds a patch between two versions, and
`xpack index --delta` offers it beside the full package.

## The installer and its wizard

A person who double-clicks the installer gets a native wizard: welcome,
licence, location, a summary, progress, and an offer to open the application.
It is drawn with the system's own controls (AppKit on macOS, Win32 on Windows),
so it follows dark mode and accessibility settings.

The wizard is configured with a small JSON file. Every setting is optional:

```json
{
  "license": "LICENSE.txt",
  "pages": ["welcome", "license", "location", "ready", "install", "finish"],
  "text": { "welcome": "This will install {name} {version} for your user account." },
  "shortcutDefault": true,
  "launchOnFinish": true
}
```

```sh
xpack installer MyApp-1.3.0-*.xpkg --ui installer-ui.json --out-dir dist
```

The name, publisher and icon are not settings: they come from the signed
package, so the installer cannot claim to be something it is not.

**For scripts and deployment tools**, every installer also runs without a
window:

```sh
MyApp-1.3.0-windows-x64-Setup.exe --silent
"Install MyApp.app/Contents/MacOS/Install MyApp" --silent --root ~/Apps
```

| Flag | Effect |
| --- | --- |
| `--silent` | No window. |
| `--dry-run` | Report what installing would do, and exit with the code it would return. |
| `--root <DIR>` | Install somewhere other than the per-user default. |
| `--no-shortcut` | Skip the Start Menu / Applications entry, and with it the desktop shortcut (first install only). |
| `--no-desktop-shortcut` | Skip the shortcut on the desktop, which is otherwise added beside the entry (first install only). |
| `--no-path` | Skip the command the application asks for, so a terminal cannot start it by name (first install only). |
| `--log <FILE>` | Also write a log of the run. |
| `--all-users` | Install for everyone on the computer, where the installer offers it. Needs administrator rights. |
| `--only-me` | Install for the person running it only, where the installer offers the choice. |
| `--wait-for-close <SECONDS>` | When the application is open and has to be closed for the install, wait this long for it (default 60), then give up with exit code `6`, having changed nothing. |
| `--close-running` | When the application is open and has to be closed for the install, ask it to close, as closing its window would. It is never forced. |

The application has to be closed only when the installer replaces xPack's own
programs in the installation, which it does when it comes from a newer xPack
than the one that installed them. The installer's window then says the
application is open, offers to ask it to close, and installs when Install is
chosen again.

On Windows, `xpack installer` produces a windowed installer by default. A shell
does not wait for a windowed program, so build installers that only scripts
run with `--console`.

### Signing the Windows installer

Windows names the publisher of a `Setup.exe` only from an Authenticode
signature. Without one, a person running it is told the publisher is
*unknown*. The application's name, publisher and icon that xPack writes into
the installer show in Explorer's Properties, but only a signature made with
a code-signing certificate you own changes that prompt.

Give `xpack installer` the command you sign with, `{file}` standing for the
installer's path:

```sh
xpack installer MyApp-1.3.0-windows-x64.xpkg --out-dir dist \
  --sign-command "signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /f cert.pfx {file}"
```

xPack runs it on the finished installer, then checks that the installer
carries a signature and still reads and verifies its own payload. xPack never
sees the certificate, so whatever you sign with works: a `.pfx` file, a
hardware token, or a cloud service such as Azure Trusted Signing. On macOS or
Linux, sign with `osslsigncode` or `jsign` instead of `signtool`.

The command is split at spaces, with double quotes keeping a part together,
and runs without a shell, so Windows paths keep their backslashes. For a
password from the environment, as in CI, point it at a script of your own,
such as `pwsh sign.ps1 {file}`.

**Signing never stops a build.** Without `--sign-command` the installer is
unsigned, as before. With it, a signing tool that is not installed (xPack
says what to install, before building), a command that fails or one that
signs nothing all leave the installer unsigned, with a warning, and the
summary's `code signed` line says `NO`. `--json` reports `codeSigned`, for a release
that must not ship unsigned. The one exception is a command that damages the
installer so it can no longer read its payload: that installer is deleted and
the build fails, because it would not install.

A signed installer is also protected by the signature: change a byte of what
it carries and Windows refuses it before xPack's own checks run.

### Installing for everyone on the computer

By default an application is installed for the person installing it, in their
own account, with no administrator rights. **To let it be installed for every
user of the computer**, say so in the wizard settings:

```json
{ "allUsers": "offer", "allUsersDefault": false }
```

`"never"` (the default) keeps it for one person, `"offer"` puts an "Install for
everyone on this computer" box on the location page, starting from
`allUsersDefault`, and `"always"` installs for everyone. In the wizard,
choosing everyone asks for administrator rights the way the system does: the
UAC prompt on Windows, the administrator password on macOS. Linux has no
wizard: there, and from any terminal, the installer's `--all-users` needs
`sudo` or an elevated prompt, and `xpack install --all-users` installs a
package the same way.

| | Installed in | Menu entry | Command |
| --- | --- | --- | --- |
| Windows | `Program Files\<Name>` | every user's Start Menu | every user's `PATH` |
| macOS | `/Library/Application Support/<Name>` | `/Applications` | `/usr/local/bin` |
| Linux | `/opt/<name>` | `/usr/share/applications` | `/usr/local/bin` |

What changes for an installation every user shares:

- **Updates come from an administrator installing the new version.** Users
  cannot write to it, so there are no background updates and no update notice.
  A new version is active as soon as it is installed, with no probation: no
  user's start can roll it back, since none can write to the installation. The
  version before it stays on disk.
- **Each user's own files stay in their account**: logs, the startup report and
  the single-instance files (`XPACK_HEALTH_FILE` and `XPACK_INSTANCE_INBOX` name
  them there).
- **The installation is refused** unless every directory above it belongs to
  the system and nobody else can write to it, and a command is not added to a
  `/usr/local/bin` another user owns (as Homebrew sometimes leaves it). The
  package is copied where only an administrator can write before it is checked,
  and installed from that copy. `--trust-on-first-use` and `--root` do not
  apply.
- **Its own uninstaller asks for administrator rights** when a user starts it,
  including from Windows' installed-apps list (with `pkexec` on Linux).
- An installation for one person and one for everyone of the same application
  live in different places, so a person can have both, each with its own menu
  entry. The wizard does not offer "everyone" to someone who already has their
  own copy; from a terminal nothing stops it. Uninstall one first.
- A directory or `/Applications` bundle of the same name that belongs to
  another program is never written to: the install stops and says so.

### Locking an application with a password

To let only people who know a password install the application, lock it in
`xpack.json`. Both locks are off unless turned on:

```json
"protection": { "installer": true, "packages": true, "passwordEnv": "XPACK_PASSWORD" }
```

- **`installer`**: the installer asks for the password before anything else,
  and carries the application encrypted, so it cannot be taken out of the
  file without it. Build it with `xpack installer <package> --config
  xpack.json`.
- **`packages`**: every package, update and delta is sealed too, so one taken
  from the update server cannot be installed with `xpack install` either.
  Needs `installer`: an installation gets the key that opens its updates from
  the password its installer was given. With it on, `xpack pack` writes only
  the sealed package, and `delta`, `index`, `inspect` and `verify` open sealed
  packages when given the password.

The password is never written in a file. Commands read it from the variable
`passwordEnv` names (`XPACK_PASSWORD` by default), or from standard input with
`--password-stdin`, and refuse one shorter than 12 characters. A silent
install reads it from `XPACK_INSTALLER_PASSWORD` or `--password-stdin`; a
wrong one exits with code 3 and installs nothing. Under `sudo`, which drops
most of the environment, use `--password-stdin`.

The key comes from the password through Argon2id, and the package is
encrypted with XChaCha20-Poly1305. The publisher's signature is still checked
after it is opened. What this is, and is not:

- It keeps a copy of the download from being installed by someone without the
  password. It is **not** a licence system: anyone with the password can pass
  it on.
- Nothing limits guessing: the slow key derivation is all that stands between
  a copied installer and an attacker, so choose a long password.
- **Changing the password cuts every existing installation off from its
  updates, and so does turning `packages` on for installations made from an
  installer that was not locked.** Each keeps the key its installer was
  given, and an update sealed with another one, or reaching an installation
  that has none, is refused. Installations made from an installer locked with
  the same password keep updating. Reinstalling from a new installer is the
  way back.
- An installation for one user keeps the key (never the password) in its
  `config` directory, readable by that user only, to open its updates. One
  for everyone keeps none: an administrator updates it by installing again,
  with the password.

## Hooks

Some applications need more than files: a background service registered, a
database converted when a version changes, something removed when the
application is. A hook is a JavaScript file in the payload that xPack runs
**once** at a moment of an installation's life:

| Operation | Moments (`when`) | Left out |
| --- | --- | --- |
| `install` | `before` anything is written, `afterFiles` (files placed, not yet active), `after` | `after` |
| `update` | `before` the switch, `after` it, `confirmed` once the new version has started well | `after` |
| `rollback` | `before`, `after` | `after` |
| `uninstall` | `before`, `after` | `before` |

```json
"hooks": {
  "install":   { "when": "after", "script": "xpack/hooks/install-service.js" },
  "update": [
    { "when": "before",    "script": "xpack/hooks/stop-service.js" },
    { "when": "after",     "script": "xpack/hooks/start-service.js", "timeoutSeconds": 600 },
    { "when": "confirmed", "script": "xpack/hooks/remove-backup.js" }
  ],
  "uninstall": "xpack/hooks/remove-service.js",
  "permissions": {
    "user": {
      "exec":  ["systemctl"],
      "write": ["{home}/.config/systemd/user"]
    }
  }
}
```

`script` is a `.js` file of the payload, relative to it; `timeoutSeconds`
defaults to 300. A script exports `main`, which may be `async`:

```javascript
// xpack/hooks/install-service.js
export function main(ctx) {
    if (ctx.platform.os === "linux") {
        const agent = ctx.path(ctx.versionDir, "bin", "example-agent");
        const unit = `[Service]\nExecStart=${agent}\n\n[Install]\nWantedBy=default.target\n`;
        ctx.file.write(ctx.path(ctx.home, ".config/systemd/user/example.service"), unit);
        const enabled = ctx.exec("systemctl", ["--user", "enable", "--now", "example"]);
        if (enabled.exitCode !== 0) {
            throw new Error(`systemctl failed: ${enabled.stderr}`);
        }
    }
    ctx.log.info(`ready for ${ctx.toVersion}`);
}
```

One script serves every platform: the engine, QuickJS, ships with xPack as
`xpack-hook`, so nothing is installed on the user's machine, and the script
branches on `ctx.platform` where the work differs. There is no `require`, no
import, no Node or browser API and no network: only `ctx`, which gives the
operation and versions (`operation`, `when`, `fromVersion`, `toVersion`, and
`cause` for a rollback), the places (`versionDir`, `dataDir`, `tempDir`,
`home`, …), `exec` (a program run directly, never through a shell), `file`
(`exists`, `read`, `write`, `copy`, `move`, `remove`, `makeDir`, `list`) and
`log`.

**What a hook may do.** Inside the installation it may write only in its data
directory (`ctx.dataDir`, shared with the application) and its own temporary
directory. Beyond it, only what `permissions` declares, separately for an
installation for one user (`user`: places under `{home}`) and one for
everyone (`machine`): the programs it may run and the places it may write.
Paths are resolved, links included, before they are checked. A hook runs as
whoever runs the operation, never with more: in an installation for one user
it cannot elevate, and `sudo`, `runas` and the like are refused.

**When one fails.** A failed `install` hook undoes the installation and leaves
nothing behind, its log included; the installer says which hook failed and
why, and exits with code `7`. A failed `update.before` or `update.after`
rolls the version back and marks it bad. A failed `confirmed`, `rollback` or
`uninstall` hook is logged and the operation carries on. Every hook runs at
most once, even if the machine stops halfway through it, so a hook should
cope with finding some of its work already done.

**Testing before shipping.** `xpack pack` checks every hook and its
permissions; `xpack hooks check <payload>` does the same without building.
`xpack hooks test` installs, updates and uninstalls the package in throwaway
installations and runs its hooks:

```sh
xpack hooks test MyApp-1.3.0-linux-x64.xpkg --previous MyApp-1.2.0-linux-x64.xpkg
```

By default nothing a hook asks for is done: each program it would run and
each file it would change is recorded and shown, and programs answer what
`--answers` says. `--real` does it all, for a disposable machine of the
target platform such as a CI runner, never your own. `--all-users` tests an
installation for everyone. The report is written beside the package, and
**`xpack installer`, `xpack index` and `xpack delta` refuse a package with
hooks unless a passing report for that exact build is beside it**. The report
must have run the update scenario when the release has update hooks and
installations update to it, and must come from a real run when the release
is mandatory. There is no override: to ship without testing a hook,
remove it. `xpack index --current <published update tree>` also shows how a
release's hooks differ from the published one's, and refuses to publish the
change without `--accept-hook-changes`. `xpack list --hooks` shows which
hooks have run in an installation.

Hooks make the package format 6. Installations made with xPack 0.7.x or
earlier cannot update to it on their own; running an installer built with
xPack 0.8.0 or later over them brings xPack's programs up to date, the data
kept.

## Rust

`cargo xpack` ships beside `xpack`, so a Rust project is packaged with the
tools it already has. Describe the application in `Cargo.toml`:

```toml
[package.metadata.xpack]
id = "com.example.mytool"   # required: the identity installations trust
command = "mytool"          # optional: typed by name once installed
```

```sh
cargo xpack pack --key ~/keys/signing.json         # build --release, then a signed package
cargo xpack installer --key ~/keys/signing.json    # and the installer a user runs
```

`cargo xpack installer --sign-command "<command> {file}"` signs a Windows
installer, as [`xpack installer` does](#signing-the-windows-installer).

The name, version, description and publisher come from `[package]`. Optional
settings: `name`, `publisher`, `commands` (further binaries typed by name),
`keep-working-directory` (on by default with a `command`), `binaries`,
`launch`, `resources`, `icon` and `installer-ui`. Each platform wants its own
icon format, so `icon` may name one per platform, and only the one for the
platform being built is packed:

```toml
icon = { macos = "assets/mytool.icns", windows = "assets/mytool.ico", linux = "assets/mytool.png" }
```

To ship updates, name where each platform's index lives; `{platform}` becomes
the platform being built, the directory `xpack index` writes its index into:

```toml
update-url = "https://updates.example.com/mytool/{platform}"
update-channel = "stable"   # the default
```

Everything lands in `target/xpack`. `--target <triple>` builds for another C
library on the same machine, such as `x86_64-unknown-linux-musl`, and
`--no-build` packages binaries an earlier step already built and tested.

## Maven

A Maven plugin packages a Java project with a runtime linked by `jlink`,
signs it, publishes updates and builds installers:

```sh
mvn package -Pinstaller
```

See [`integrations/maven`](integrations/maven/README.md), and the complete
example project in
[`integrations/maven/src/it/bundled-jdk-app`](integrations/maven/src/it/bundled-jdk-app).
Hooks are declared in the POM, and `xpack:hooks-test` tests them as part of
an ordinary build. `<windowsSignCommand>` signs the Windows installer, as
[`xpack installer --sign-command`](#signing-the-windows-installer) does. The
plugin is on Maven Central as `io.github.zaaim-halim:xpack-maven-plugin`.

[Expense Tracker](https://github.com/Zaaim-Halim/expense-tracker) is a real
JavaFX application built, released and updated this way: CI builds it with
the plugin on each platform, publishes its releases and update index, and
installed copies update themselves from there. It is the reference
application xPack is validated against.

## Security

- **Packages** are signed with Ed25519; every file is checked against the
  signed manifest before it is installed.
- **Keys are pinned.** The first install records the publisher's key, and every
  later update must be signed by it. An installer carries the key it expects,
  so even the first install cannot be substituted.
- **No downgrades.** An update server cannot push an older, vulnerable version.
- **Nothing runs elevated** unless the application is [installed for
  everyone](#installing-for-everyone-on-the-computer), which asks for
  administrator rights once, to install; the application itself always runs
  as the user.
- **Hooks are trusted code, not sandboxed code.** Their scripts are payload
  files, verified with the rest before anything runs, and `ctx` holds them to
  the places and programs they declare. But a program a hook may run can do
  whatever the account running it can. Permissions make hooks reviewable and
  catch mistakes; they do not contain a publisher who means harm.
- **Sign your installers.** On Windows, `--sign-command` signs the installer
  with your certificate as it is built ([how](#signing-the-windows-installer)).
  On macOS, sign and notarise the installer bundle after `xpack installer`.

## Commands

| Command | Does |
| --- | --- |
| `keygen` | Generate a signing key pair |
| `pack` | Build a signed `.xpkg` from a payload directory |
| `inspect` | Describe a package without trusting it |
| `verify` | Check a package's signature and files against a key |
| `installer` | Build a self-contained installer from a package |
| `index` | Write the update index a server publishes |
| `delta` | Build a differential update between two packages |
| `install` | Install a package |
| `list` | List installed versions |
| `run` | Launch the active version |
| `update` | Check for and apply an update |
| `activate` | Make an installed version active |
| `rollback` | Return to the previous healthy version |
| `prune` | Remove versions that are no longer needed |
| `uninstall` | Remove an installation |
| `recover` | Finish or undo an interrupted operation |
| `autoupdate` | Turn automatic update checks on or off |
| `hooks` | Check a project's hooks, or test a package's in throwaway installations |

`xpack <command> --help` describes each one.

**Exit codes**, the same for `xpack` and every installer: `0` success, `1`
failure, `2` bad command line, `3` a signature or checksum did not verify
(never worth retrying), `4` another xPack operation is busy (worth retrying),
`5` the user cancelled the installer, `6` the application is open and has to
be closed for the install (nothing was changed), `7` a hook of the package
failed and the operation was undone.

## Building and contributing

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All four must pass. The workspace forbids `unsafe` code outside the few
platform calls that need it, each of which documents why it is sound.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
