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
| `xpack:delta` | `verify` | Builds differential updates against a published release |
| `xpack:index` | `deploy` | Writes the documents an update server publishes |
| `xpack:installer` | `package` | Builds the artefact a user runs on a bare machine |
| `xpack:run` | — | Installs into a throwaway root and launches; the developer loop |

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
