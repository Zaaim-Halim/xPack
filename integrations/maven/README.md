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

`xpack:installer` then rewrites the installer, launcher, updater and
uninstaller it ships: each gets the application's name, the version, the
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
