# xpack-maven-plugin

Packages a Maven project into a signed, self-updating xPack installation with
a bundled Java runtime.

```
io.xpack:xpack-maven-plugin
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

## Windows icons and names

On Windows the icon Explorer draws and the name Task Manager shows live
inside the executable. Everywhere else they do not exist there at all — the
`.app` bundle and the `.desktop` entry carry them, from the icon the manifest
already names — so this is one platform's problem and not a gap on the
others.

```xml
<icon>${project.basedir}/src/main/resources/icon.png</icon>
```

`xpack:installer` then rewrites the installer, launcher, updater, uninstaller
and update dialog it ships: each gets the application's name, the version, the
publisher, and a description saying which of them it is. A `.png` is expanded
into the sizes Windows chooses between, so the same file can serve
`<desktop><icon>` rather than being a second one to keep in step.

Nothing is rewritten on macOS or Linux, and the flag is ignored there.

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

## Quickstart

Needs a JDK with `jlink`, the `xpack` binary on `PATH` (or `-Dxpack.home=`),
and a signing key from `xpack keygen --out xpack-signing.json`.

```xml
<plugin>
  <groupId>io.xpack</groupId>
  <artifactId>xpack-maven-plugin</artifactId>
  <version>0.1.0-SNAPSHOT</version>
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
