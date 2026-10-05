# xpack-maven-plugin

Packages a Maven project into a signed, self-updating xPack installation with
a bundled Java runtime.

```
io.github.zaaim-halim:xpack-maven-plugin
```

## What it does, and what it refuses to do

The plugin is glue. It knows what Maven knows — the jar, the dependencies, the
version, the main class — and turns that into a payload directory and an
`xpack.json`. Then it runs the xPack command line.

It does not implement packaging, signing, hashing or the index format. Those
have one implementation, and a second one living in a build plugin would be a
place for a security fix to be missed.

## Goals

| Goal | Default phase | Does |
| --- | --- | --- |
| `xpack:runtime` | `prepare-package` | Links a runtime with `jlink`, one per target |
| `xpack:payload` | `package` | Copies the jar and its runtime dependencies into the payload |
| `xpack:manifest` | `package` | Writes `xpack.json` |
| `xpack:pack` | `package` | Builds a signed `.xpkg` |
| `xpack:hooks-test` | `integration-test` | Runs the release's hooks with `xpack hooks test` and leaves the report beside each package |
| `xpack:delta` | `verify` | Builds differential updates against releases in the repository |
| `xpack:index` | `deploy` | Writes the documents an update server publishes |
| `xpack:installer` | — | Builds the artefact a user runs on a bare machine |
| `xpack:run` | — | Installs into a throwaway root and launches; the developer loop |

## Two cadences

An installer is downloaded once, by a new user. A package and its deltas are
what every existing user receives. The goals are bound accordingly.

**Every release** — bound to the lifecycle, so `mvn deploy` does the whole
thing:

```
xpack:runtime → jar:jar → xpack:payload → xpack:manifest → xpack:pack
                                        → xpack:hooks-test  (integration-test)
                                        → xpack:delta  (verify)
                                        → xpack:index  (deploy)
```

**Occasionally** — run by hand when you want a new installer, typically once
per major release or when the xPack binaries themselves change:

```sh
mvn xpack:installer
```

`xpack:installer` is deliberately not bound to a phase. An installer carries
the whole package plus the runtime binaries and changes almost never; building
one on every commit is tens of megabytes of nothing.

To build it in the same run as everything else when you do want one, bind it
in a profile, so `mvn package -Pinstaller` makes the package and the setup
file together and a plain `mvn package` stays as it was:

```xml
<profiles>
  <profile>
    <id>installer</id>
    <build>
      <plugins>
        <plugin>
          <groupId>io.github.zaaim-halim</groupId>
          <artifactId>xpack-maven-plugin</artifactId>
          <executions>
            <execution>
              <id>installer</id>
              <phase>package</phase>
              <goals><goal>installer</goal></goals>
            </execution>
          </executions>
        </plugin>
      </plugins>
    </build>
  </profile>
</profiles>
```

It runs after `xpack:pack`, which it needs: goals bound to the same phase run
in the order they are declared, and a profile's executions come after the main
build's. The demo project in `src/it/bundled-jdk-app` does exactly this.

## The runtime is shipped once, not every release

`xpack:pack` puts a full runtime in every package — a package is
self-contained and signed as a whole, so it cannot reference one it does not
carry. That sounds like it means shipping 50 MB per release. It does not.

`xpack:delta` builds a differential update: the files whose contents changed,
and nothing else. The rest is reused from the version already on the user's
disk and re-hashed against the new release's signed manifest as it is copied.
A release that changed one jar measures:

```
xpack: delta 1.0.0 -> 1.1.0 (linux-arm64)  1 changed, 95 reused, 23.8 KiB
```

23.8 KiB, against a 36 MB package. The runtime cost nothing, because `jlink`
is deterministic: re-linking it every build produces byte-identical files, so
the delta sees no change. Upgrade the JDK and the runtime ships once, which is
correct.

The earlier packages come from the repository. `xpack:pack` attaches each one
to the project, so `mvn deploy` publishes them beside the jar with the
platform as a classifier, and the next release resolves them back:

```xml
<!-- the default: build deltas from the last three releases -->
<lastReleases>3</lastReleases>
```

A user further behind than that downloads the full package once, which is the
right outcome rather than a failure.

**A build fails if it should have produced a delta and did not.** A delta is
an optimisation for the *client* — one that fails at install time falls back
to the full package and nothing breaks. A *publisher* shipping none is a
different thing: every user on the previous release downloads the whole
runtime again, on a release whose build log said success. So an unreachable
repository, or an earlier release whose package was never published, stops
the build:

```
no published package for 1.0.0 (linux-arm64), so users of those releases
would download the whole package again. Publish them, narrow <lastReleases>,
or set -Dxpack.delta.required=false.
```

Having nothing to build from is not that, and never fails: a first release has
no earlier version.

Attaching can be turned off with `-Dxpack.attach=false`, at the cost of having
to supply earlier packages yourself through `<deltaFromFiles>`.

## Where the packages are served from

`xpack:index` writes one `stable.json` per platform into
`target/xpack/site`. By default each names its package by file name, so you
upload the package beside it: one directory per platform, served from the
`<update><url>` in the manifest.

To keep the packages somewhere else, such as attached to a GitHub Release
while the small index files go to a static site, name where they are
downloaded from:

```xml
<packageUrl>https://github.com/example/myapp/releases/download/v${project.version}</packageUrl>
```

Each package is then named by its full address. It must be `https`.

## Windows icons and names

On Windows the icon Explorer draws and the name Task Manager shows live
inside the executable. `xpack:installer` rewrites the installer, launcher,
updater, uninstaller and update dialog it ships: each gets the application's
name, version and publisher, a description saying which one it is, and the
icon.

The icon is the one `<desktop><icon>` names: the same file the Start Menu
entry, the `~/Applications` bundle and the `.desktop` entry use, and the one
the macOS installer bundle now shows in Finder. There is nothing else to set.

```xml
<desktop>
  <icon>icon.png</icon>
</desktop>
```

`<icon>` on `xpack:installer` is **deprecated**. It is still passed on and
still wins, so existing builds are unchanged, and it logs a warning.

## Signing the Windows installer

The name above shows in Explorer's Properties, but Windows still calls the
publisher of an unsigned `Setup.exe` *unknown* when someone runs it. Only an
Authenticode signature, made with a code-signing certificate you own, changes
that. Give `xpack:installer` the command you sign with, with `{file}` where
the installer's path goes:

```xml
<configuration>
  <windowsSignCommand>signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /f cert.pfx {file}</windowsSignCommand>
  <!-- or -Dxpack.windowsSignCommand=... -->
</configuration>
```

The plugin passes it to `xpack installer --sign-command`, which runs it on
each finished Windows installer, then checks the installer carries a
signature and still reads its own payload. xPack never sees the certificate,
so a `.pfx`, a hardware token or a cloud signing service all work.
Installers for other platforms in the same build are made as before.

Signing never stops the build. Without `<windowsSignCommand>` the installer
is built unsigned, as always. With it, a signing tool that is not installed
(xPack says what to install), a command that fails, or one that signs
nothing leaves the installer unsigned with a warning, `NOT code signed`, and
the build goes on. The one exception is a command that damages the
installer so it can no longer read its own payload: that installer is
deleted and the build fails, because it would not install.

The command is split at spaces, double quotes keep a part together, and it
runs without a shell. For a password from the environment, as in CI, point it
at a script of your own that reads it, such as
`pwsh sign.ps1 {file}` or `bash sign.sh {file}`.

Needs xPack 0.8.1 or later, the command line and the plugin alike.

## The installation wizard

Installers open a wizard for a person who double-clicks them: the macOS
bundle, and on Windows the windowed build `xpack:installer` now produces by
default. Scripts pass `--silent`, and a Windows installer only scripts run can
be built on the console build instead:

```xml
<configuration>
  <console>true</console>   <!-- or -Dxpack.installer.console=true -->
</configuration>
```

What the wizard shows is set in `<installerUi>`. Every setting has a default,
and so does the block: leave it out for the recommended wizard.

```xml
<installerUi>
  <license>${project.basedir}/LICENSE.txt</license>
  <pages>
    <page>welcome</page><page>license</page><page>location</page>
    <page>ready</page><page>install</page><page>finish</page>
  </pages>
  <text>
    <welcome>This will install {name} for your user account.</welcome>
  </text>
  <shortcutDefault>true</shortcutDefault>
  <launchOnFinish>true</launchOnFinish>
</installerUi>
```

| Setting | Default | Notes |
| --- | --- | --- |
| `license` | none | UTF-8 text, at most 256 KiB. Without one there is no licence page |
| `pages` | every page with something to show | Chooses which pages appear, never their order |
| `text` | English defaults | Only `welcome` and `finish`, using `{name}` and `{version}` |
| `shortcutDefault` | `true` | Whether the Start Menu / Applications box starts ticked |
| `pathDefault` | `true` | Whether the command-line box starts ticked, when `<command>` is set |
| `allUsers` | `never` | `offer` lets the person install for everyone on the computer; `always` does so. Needs administrator rights, which the installer asks for |
| `allUsersDefault` | `false` | Whether "everyone on this computer" starts ticked, with `offer` |
| `launchOnFinish` | `false` | Whether the last page offers to open the application |

The name, publisher and icon are not settings: they come from the signed
package. The plugin checks none of this itself; the xPack command line does,
and a misspelt or unknown setting fails the build with its message.

## Applications that do not bundle a runtime

Some applications should use the interpreter the machine already has.

```xml
<runtime>
  <bundled>false</bundled>
  <command>java</command>
</runtime>
```

The launch executable becomes a bare name, which the operating system looks up
on `PATH` when the application starts. The package is then the application
alone — 5 KiB rather than 36 MB — and the application is at the mercy of
whichever version it finds, and of whether it is there at all. That is the
trade; bundling exists to avoid it.

## Command-line tools

An application starts in its installed version directory, which is what a
program opened from a menu, Finder or Explorer wants.

**Set `<keepWorkingDirectory>true</keepWorkingDirectory>` when the application
is also used from a command line**: a command-line tool, or an application that
takes file paths in a terminal (`mytool build src`, `myeditor notes.txt`). It
then starts in the directory the user ran it from, so the paths they type mean
what they meant.

```xml
<keepWorkingDirectory>true</keepWorkingDirectory>   <!-- or -Dxpack.keepWorkingDirectory=true -->
```

The plugin then names the application's jars as `{versionDir}/application/*`;
the launcher replaces `{versionDir}` with the installed version directory, so
they are still found. A `<jvmArg>` or `<appArg>` of your own that names a file
in the package needs the same, by hand:
`-Djava.library.path={versionDir}/native`.

To let users type it by name, give it a command:

```xml
<command>mytool</command>   <!-- or -Dxpack.command=mytool -->
```

The installer offers "Add “mytool” to the command line", ticked unless
`<installerUi><pathDefault>false</pathDefault></installerUi>`; a silent
install declines it with `--no-path`. It becomes a script in `~/.local/bin`
on macOS and Linux and a `PATH` entry on Windows, never replaces another
program's `mytool`, and is removed on uninstall.

Each of these makes the package format 2, which installations made with xPack
0.1.0 cannot install or update to. Leave them off for a windowed application.

## Locking with a password

```xml
<protection>
  <installer>true</installer>   <!-- the installer asks for the password -->
  <packages>true</packages>     <!-- every package, update and delta is sealed -->
  <passwordEnv>XPACK_PASSWORD</passwordEnv>
</protection>
```

Both are off by default, and `<packages>` needs `<installer>`. The password
is read from `XPACK_PASSWORD`, or the variable `<passwordEnv>` names, in the
build's environment (a CI secret), never from the POM. With `<packages>`,
`xpack:pack` attaches only the sealed package, and `xpack:delta` opens sealed
earlier releases with the same password. Changing the password, or turning
`<packages>` on for an application already released, cuts existing
installations off from their updates. See
[Locking an application with a password](../../README.md#locking-an-application-with-a-password).

## Hooks

```xml
<hooks>
  <install>
    <hook><script>install-service.js</script><when>after</when></hook>
  </install>
  <update>
    <hook><script>stop.js</script><when>before</when></hook>
    <hook><script>start.js</script><when>after</when><timeoutSeconds>600</timeoutSeconds></hook>
  </update>
  <permissions>
    <user>
      <exec><program>systemctl</program></exec>
      <write><place>{home}/.config/systemd/user</place></write>
    </user>
  </permissions>
</hooks>
```

Scripts live in `src/xpack/hooks/` (`<hooksDirectory>` moves it), which
`xpack:payload` copies into the payload at `xpack/hooks/`, so `<script>` names
a file relative to that directory. `<when>` is the moment it runs at:

| Operation | Allowed | Left out |
| --- | --- | --- |
| `install` | `before`, `afterFiles`, `after` | `after` |
| `update` | `before`, `after`, `confirmed` | `after` |
| `rollback` | `before`, `after` | `after` |
| `uninstall` | `before`, `after` | `before` |

`before` runs before anything changes; `afterFiles` once the files are in
place, before the version is active; `confirmed` once the updated version has
started well. Everything is passed to `xpack pack`,
which checks every rule and names what is wrong; the plugin checks none.

A release with hooks ships only once they have passed: `xpack:installer`,
`xpack:delta` and `xpack:index` refuse a package without a passing
`xpack hooks test` report for that exact build beside it. `xpack:hooks-test`
writes that report, at `integration-test`, so an ordinary `mvn verify` or
`mvn deploy` tests before it ships. It runs:

- in plan mode, doing nothing the hooks ask for, unless
  `-Dxpack.hooks.real=true`: for a disposable machine of the target platform,
  such as a CI runner, never your own. A mandatory release needs a real run.
- the update scenario over the latest release in the repository, or
  `<previousVersion>`, or `<previousFiles>` named directly.
- for an installation for everyone too, when `<installerUi><allUsers>` is
  `offer` or `always`.

A profile that builds the installer at `package` declares `hooks-test` in the
same phase, before `installer`, as the sample's `installer` profile does.

`xpack:index` compares a release's hooks with the published one's when given
`-Dxpack.index.current=<directory or https URL>`, the update tree as
published, and refuses changes unless `-Dxpack.index.acceptHookChanges=true`. See
[Hooks](../../README.md#hooks) for what a hook may do and how it is tested.

## One running copy

To keep one copy of the application running for each user:

```xml
<singleInstance>true</singleInstance>   <!-- or -Dxpack.singleInstance=true -->
```

A second start passes its arguments to the running copy and exits. The running
copy finds them as `.json` files, `{"arguments": [...]}`, in the directory
named by `XPACK_INSTANCE_INBOX`, which a `WatchService` can follow. See
[One running copy](../../README.md#one-running-copy) for what each platform
does and what it cannot. It makes the package format 4, which installations
made with xPack 0.5.0 or earlier cannot install or update to.

An application that also answers on the command line lists the arguments that
are commands, so they run beside the open window with their own output and
exit code instead of being handed over (and printing nothing):

```xml
<singleInstanceAlongside>
  <argument>--version</argument>
  <argument>--status</argument>
</singleInstanceAlongside>
```

Each entry is one argument as typed, and also matches it with a value
(`--export` matches `--export=out.csv`). It makes the package format 5, which
installations made with xPack 0.6.x or earlier cannot install or update to.

## Quickstart

Needs a JDK with `jlink`, the `xpack` binary on `PATH` (or `-Dxpack.home=`),
and a signing key from `xpack keygen --out xpack-signing.json`.

```xml
<plugin>
  <groupId>io.github.zaaim-halim</groupId>
  <artifactId>xpack-maven-plugin</artifactId>
  <version>0.8.1</version>
  <configuration>
    <mainClass>com.example.demo.Main</mainClass>
    <runtime>
      <modules>
        <module>java.base</module>
        <module>java.desktop</module>
      </modules>
    </runtime>
  </configuration>
  <executions>
    <execution>
      <goals>
        <goal>runtime</goal>
        <goal>payload</goal>
        <goal>manifest</goal>
        <goal>pack</goal>
        <goal>delta</goal>
        <goal>index</goal>
      </goals>
    </execution>
  </executions>
</plugin>
```

```sh
mvn -Dxpack.key=$PWD/xpack-signing.json package
mvn -Dxpack.key=$PWD/xpack-signing.json xpack:run
```

A working example is in `src/it/bundled-jdk-app`.

## Checking for updates while the application runs

By default an installation checks once, when the application starts, and
applies whatever it found the next time it starts. Nothing runs in between and
the user is told nothing.

`<update>` changes that:

```xml
<configuration>
  <updateBaseUrl>https://updates.example.com/demo</updateBaseUrl>
  <update>
    <channel>stable</channel>
    <checkWhileRunning>true</checkWhileRunning>
    <checkIntervalMinutes>180</checkIntervalMinutes>
    <notify>true</notify>
    <severity>recommended</severity>
    <prompt>
      <title>A new version is ready</title>
      <message>It has been downloaded and checked already.</message>
    </prompt>
  </update>
</configuration>
```

`<checkWhileRunning>` is the switch. Off, the application is checked only when
it starts. On, the launcher keeps looking for as long as the application is
open — which is not a promise that anything happens on a schedule, since an
application nobody opens is never checked.

`<checkIntervalMinutes>` says how often the update server may be asked, and
nothing else. It governs the check made at startup too, so it is worth setting
even when nothing looks while running. Unset means four hours; below fifteen
minutes is raised to fifteen. **Leaving it out cannot turn checking off** —
only the switch does that.

`<notify>` decides whether a check that finds something may say so, and
`<prompt>` is what it says. That wording travels inside the signed manifest, so
it is the publisher's text rather than whatever the update server served.
`<severity>` is `optional`, `recommended` or `critical`; it decides what the
prompt offers rather than what is installed, so a critical release is told, not
offered. It is separate from `<mandatory>`, which refuses to let anything older
start once the release is installed. Set both for a security release.

## Which cadence decides what

The two cadences above are not only a build schedule. They decide which
settings an existing user can be given and which ones only a new installer can
deliver.

### When you build the installer

```sh
mvn xpack:installer
```

The `<update>` block in the POM **at that moment** is packaged into the setup
file, along with the binaries that installation will run for the rest of its
life. One of those binaries is the update dialog, and it is shipped only when
the POM asks for one:

```xml
<update>
  <checkWhileRunning>true</checkWhileRunning>   <!-- there is a check to speak up from -->
  <notify>true</notify>                         <!-- and the user may be told -->
</update>
```

Both, plus a platform with a dialog implemented — Windows and macOS — and the
installer carries `<App> Update Notice`. Either one missing and it does not,
and the installation has no graphical code in it at all. That is deliberate:
an application that updates silently should not ship a dialog it never opens.

Everything else in the block is packaged too, but nothing about it is
special — a later release can change all of it.

### When you ship a regular update

```sh
mvn deploy
```

The new release carries its own manifest, and its `<update>` block is what
existing installations obey from the moment they install it:

- `<checkWhileRunning>` can be turned on or off. The checking is done by the
  launcher and the updater, which every installation already has, so this takes
  effect on the next start with nothing new to install.
- `<checkIntervalMinutes>` can be raised or lowered. Ship a busy release and
  lower it; leave it out and it returns to four hours.
- `<severity>`, `<prompt>` and `<mandatory>` describe *that* release, and are
  read from it when it is staged. A release announces itself in its own words.

**`<notify>` is the one that cannot be turned on this way.** Showing a dialog
needs the binary, and a background update runs from inside the installation
with no copy of one to place. Set it in a release whose installations were made
without it and nothing happens: the update is applied quietly at the next start
and the installation's log says why. Adding it takes a new installer, which
users have to download.

So the rule of thumb: **decide about prompting when you build the installer,
and tune everything else per release.**

| | Set in | An existing installation can be given it |
|---|---|---|
| `<checkWhileRunning>`, `<checkIntervalMinutes>` | every release | yes |
| `<severity>`, `<prompt>`, `<mandatory>` | every release | yes |
| whether a dialog exists at all | the **installer** | **no** |

`<updateBaseUrl>` stays outside the block so that `-Dxpack.updateBaseUrl` can
override it from a release build without editing the POM.

## Things that will bite

- **Cross-building needs the target platform's JDK.** A runtime image is made
  of that platform's modules; the local JDK only supplies the linker. Either
  set `<targetJdks>` or build each target on its own machine in CI.
- **Build Unix targets on Unix.** A Windows host records no permission bits,
  so a bundled interpreter arrives without `+x` and the package installs but
  cannot start.
- **`<updateBaseUrl>` must be HTTPS**, except for loopback addresses, which
  are allowed so a local test server works.
- **Sign and notarise installers after `xpack:installer`**, not before —
  appending a payload invalidates a signature applied to the stub.

## Layout

```
io.xpack           the goals; the only classes Maven binds to
io.xpack.config    types bound from a POM, so renaming a field is a breaking change
io.xpack.internal  helpers, free to change: the CLI wrapper, the manifest
                   writer, JSON, process handling, target and JDK handling
```

The plugin has no runtime dependencies. JSON is a few hundred lines in
`io.xpack.internal.Json` rather than a library, because a packaging plugin
sits on the build classpath of every project that uses it and a version
conflict there is somebody else's broken build.

## Design notes

A longer document covering why the plugin is shaped this way, how to extend
it, and a full configuration reference lives in this repository at
`docs/MAVEN-PLUGIN.md`. That directory is deliberately untracked, so it is
present in a working copy rather than in a clone.
