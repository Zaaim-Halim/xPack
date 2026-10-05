# XPack.Build

Packages a .NET application into a signed, self-updating
[xPack](https://github.com/Zaaim-Halim/xPack) installation, as part of
`dotnet publish`.

```xml
<PropertyGroup>
  <XPackId>com.example.myapp</XPackId>
</PropertyGroup>

<ItemGroup>
  <PackageReference Include="XPack.Build" Version="0.8.1" PrivateAssets="all" />
</ItemGroup>
```

```sh
xpack keygen --out ~/keys/signing.json          # once
dotnet publish -c Release -r win-x64 --self-contained -p:XPackKey=$HOME/keys/signing.json
```

The signed package lands in `bin/xpack/`, for example
`My-App-1.2.0-windows-x64.xpkg`.

## What it does, and what it does not

The package is glue. It knows what MSBuild knows — the name, version,
publisher, executable and target platform — writes that into an
`xpack.json`, and runs the xPack command line. Packaging, signing, installing
and updating have one implementation, in `xpack`; nothing of it is redone
here.

Nothing happens until `XPackId` is set, and only a publish packs: a plain
`dotnet build` is untouched.

## Requirements

- The xPack command line, the same release as this package (0.8.1 with
  0.8.1). On the `PATH`, or `XPackHome` set to the folder that holds it. A
  different release is a warning naming both.
- A **self-contained** publish for one runtime identifier (`-r`). The
  application then carries its own .NET runtime, and installs and runs on a
  machine that has none.

## Platforms

| Runtime identifier | xPack platform |
| --- | --- |
| `win-x64` | `windows-x64` |
| `osx-x64` | `macos-x64` |
| `osx-arm64` | `macos-arm64` |
| `linux-x64` | `linux-x64` |
| `linux-arm64` | `linux-arm64` |

One publish makes one platform's package. From macOS or Linux, any of them
can be built. From Windows, only `win-x64`: Windows does not record the Unix
permission bits that make an application executable, so a Linux or macOS
package built there would install but not start, and it is refused before
publishing. Native AOT publishes only for the machine's own OS.

## Settings

Everything has a default taken from the project.

| Property | Default | Becomes |
| --- | --- | --- |
| `XPackId` | — (required) | the identity installations trust, e.g. `com.example.myapp` |
| `XPackKey` | — (required) | path to the private signing key; keep it outside the project |
| `XPackHome` | `xpack` on the `PATH` | the folder holding the `xpack` command line |
| `XPackName` | `Product`, else `AssemblyName` | the name users see |
| `XPackVersion` | `Version` | the version; SemVer (`1.2.0`, `1.2.0-beta.1`), not four parts |
| `XPackDescription` | `Description` | a one-line description |
| `XPackPublisher` | `Company` | the publisher users see |
| `XPackExecutableName` | `AssemblyName` (`.exe` on Windows) | the program the installation starts, relative to the publish folder |
| `XPackArgument` items | none | arguments it is started with |
| `XPackTerminal` | `false` for `WinExe`, WPF, Windows Forms; `true` for a console `Exe` | whether the shortcut opens a terminal |
| `XPackCommand` | none | a name to type in a terminal, e.g. `mytool` |
| `XPackKeepWorkingDirectory` | on with `XPackCommand` | start where the user ran it, not in the installation |
| `XPackShortcut` | `true` | a Start Menu / Applications / menu entry |
| `XPackIcon`, `XPackIconWindows`, `XPackIconMacos`, `XPackIconLinux` | `ApplicationIcon` on Windows | the icon: `.ico`/`.png` Windows, `.icns` macOS, `.png`/`.svg` Linux |
| `XPackUpdateUrl` | none | where this platform's update index lives; `{platform}` becomes e.g. `macos-arm64` |
| `XPackUpdateChannel` | `stable` | the update channel |
| `XPackSingleInstance` | `false` | keep one copy running per user |
| `XPackIncludeSymbols` | `false` | ship `.pdb` files too |
| `XPackDistDirectory` | `bin/xpack/` | where packages are written |

An Avalonia or MAUI application built as `Exe` is packaged as a window, with
a warning; set `XPackTerminal` to say which it is.

## Trying it

```sh
dotnet publish -c Release -r osx-arm64 --self-contained -p:XPackKey=$HOME/keys/signing.json -t:XPackRun
```

Publishes, packs, installs into a throwaway folder under `obj/`, and starts
the installed application.

## Signing the application's own `.exe`

`pack` records the exact bytes of every file, so anything that changes the
payload — Authenticode-signing your `.exe`, say — happens before it. Hook the
empty `XPackBeforePack` target; the payload is in `$(XPackPayloadDirectory)`:

```xml
<Target Name="SignMyExe" AfterTargets="XPackStagePayload" BeforeTargets="XPackBeforePack">
  <Exec Command="signtool sign /fd sha256 /f cert.pfx &quot;$(XPackPayloadDirectory)MyApp.exe&quot;" />
</Target>
```

## Not yet

Building the installer a user runs, deltas and the update index come in
later releases of this package. Until then, run `xpack installer` on the
package this one makes.
