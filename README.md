# xPack

**Package, install, update and roll back desktop and command-line applications
on Windows, macOS and Linux — whatever language they are written in.**

xPack turns a directory of files into a signed package, installs it for the
current user without administrator rights, keeps it up to date in the
background, and puts the previous version back if a new one fails to start.
It ships as a handful of small native binaries written in Rust; nothing of
xPack becomes part of your application.

> **Status: pre-release (0.1.0).** The command line, installing, updating and
> rollback are implemented, and the test suite runs on Windows, macOS and Linux
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
| `launch` | The executable to start, relative to the payload (a bundled runtime) or a bare name found on `PATH` (a system one), with its arguments and environment. It starts in its installed version directory; a command-line tool sets `"keepWorkingDirectory": true` to start where the user ran it, so the paths they type mean what they meant. Installations made with xPack 0.1.0 cannot update to a package that sets it. |
| `update` | Where this platform's update index lives, and the channel to follow. `mandatory` stops older versions from starting. |
| `health` | How long a new version has to prove it starts. With `requireStartupReport`, the application must write the file named in `XPACK_HEALTH_FILE`. |
| `desktop` | A Start Menu entry, a `~/Applications` bundle or a `.desktop` file, and the icon: `.ico` or `.png` on Windows, `.icns` on macOS, `.png` or `.svg` on Linux. |

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

Installed applications check in the background and apply the update on the
next start. You can also trigger it by hand:

```sh
xpack update com.example.myapp
xpack rollback com.example.myapp     # back to the previous healthy version
```

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
| `--no-shortcut` | Skip the Start Menu / Applications entry (first install only). |
| `--log <FILE>` | Also write a log of the run. |

On Windows, `xpack installer` produces a windowed installer by default. A shell
does not wait for a windowed program, so build installers that only scripts
run with `--console`.

## Maven

A Maven plugin packages a Java project with a runtime linked by `jlink`,
signs it, publishes updates and builds installers:

```sh
mvn package -Pinstaller
```

See [`integrations/maven`](integrations/maven/README.md), and the complete
example project in
[`integrations/maven/src/it/bundled-jdk-app`](integrations/maven/src/it/bundled-jdk-app).
The plugin is not yet published to Maven Central; install it locally with
`mvn install` in `integrations/maven`.

## Security

- **Packages** are signed with Ed25519; every file is checked against the
  signed manifest before it is installed.
- **Keys are pinned.** The first install records the publisher's key, and every
  later update must be signed by it. An installer carries the key it expects,
  so even the first install cannot be substituted.
- **No downgrades.** An update server cannot push an older, vulnerable version.
- **Nothing runs elevated.** Installations are per user.
- **Sign your installers.** Code-sign and notarise installer files after
  `xpack installer` — appending the payload invalidates an earlier signature.

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

`xpack <command> --help` describes each one.

**Exit codes**, the same for `xpack` and every installer: `0` success, `1`
failure, `2` bad command line, `3` a signature or checksum did not verify
(never worth retrying), `4` another xPack operation is busy (worth retrying),
`5` the user cancelled the installer.

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
