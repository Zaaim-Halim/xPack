# xPack DotNet Demo

A console application packaged with every setting XPack.Build has, each
explained in `Demo.csproj`: its identity, how it starts, a command to type in
a terminal, the desktop entry and its icon, updates, and the installation
wizard. It brings its own .NET runtime, so it installs and runs on a machine
with none.

## Build it

From this directory, with the .NET 10 SDK, and the xPack binaries built
(`cargo build --workspace` at the repository root):

```sh
# Once: XPack.Build, packed from this repository into ../feed, where
# nuget.config looks for it.
dotnet pack ../XPack.Build -c Release -o ../feed

# Once: a signing key, kept outside this project.
<repository>/target/debug/xpack keygen --out ~/xpack-dotnet-demo/keys/signing.json

dotnet publish -c Release -r osx-arm64 --self-contained \
  -p:XPackKey=$HOME/xpack-dotnet-demo/keys/signing.json \
  -p:XPackHome=<repository>/target/debug \
  -p:XPackInstaller=true
```

Use your machine's runtime identifier: `osx-arm64`, `osx-x64`, `win-x64`,
`linux-x64` or `linux-arm64`. Everything lands in `bin/xpack/`:

- the package, `xPack-DotNet-Demo-1.0.0-<platform>.xpkg`, which is what
  installed copies update from;
- the installer a person runs: `Install xPack DotNet Demo.app` on macOS,
  `xPack-DotNet-Demo-1.0.0-windows-x64-Setup.exe` on Windows,
  `xPack-DotNet-Demo-1.0.0-linux-x64-installer` on Linux. It opens a wizard
  with the licence in `LICENSE.txt`, or installs with `--silent`.

Without `-p:XPackInstaller=true` only the package is built, which is what most
releases need. From macOS or Linux any platform can be built; a Windows
machine builds Windows packages only.

Once XPack.Build is published on NuGet.org, the `pack` step and
`nuget.config` are no longer needed.

## Try it without installing

```sh
dotnet publish -c Release -r osx-arm64 --self-contained \
  -p:XPackKey=$HOME/xpack-dotnet-demo/keys/signing.json \
  -p:XPackHome=<repository>/target/debug \
  -t:XPackRun
```

Publishes, packs, installs into a throwaway folder under `obj/`, and starts
it. It prints what xPack gave it: the argument from `Demo.csproj`, its own
.NET, where it was installed, and the directory it was started in.

## Ship an update

Releases go out with deltas built from the packages users already have, and
an update index installed copies read. To watch one on this machine, point
the update URL at a local server for both releases (`127.0.0.1` is the one
address allowed without HTTPS):

```sh
key=$HOME/xpack-dotnet-demo/keys/signing.json
xpack=<repository>/target/debug
url=-p:XPackUpdateUrl=http://127.0.0.1:8765/{platform}

# 1.0.0: build it with its installer, install it, and keep its package.
dotnet publish -c Release -r osx-arm64 --self-contained -p:XPackKey=$key -p:XPackHome=$xpack \
  -p:XPackInstaller=true $url
"bin/xpack/Install xPack DotNet Demo.app/Contents/MacOS/Install xPack DotNet Demo" --silent
mkdir -p ~/xpack-dotnet-demo/releases && cp bin/xpack/*-1.0.0-*.xpkg ~/xpack-dotnet-demo/releases/

# 1.1.0: a release, with a delta from 1.0.0 and the index, in bin/xpack/site.
dotnet publish -c Release -r osx-arm64 --self-contained -p:XPackKey=$key -p:XPackHome=$xpack \
  -p:Version=1.1.0 -p:XPackRelease=true -p:XPackPreviousPackages=$HOME/xpack-dotnet-demo/releases $url

# Serve it, and update.
python3 -m http.server 8765 --bind 127.0.0.1 --directory bin/xpack/site &
$xpack/xpack update com.example.dotnetdemo
$xpack/xpack run com.example.dotnetdemo
```

The update downloads only the delta, a few dozen kilobytes instead of the
whole package, and the demo then reports 1.1.0. On their own, installed
copies check for updates in the background and apply them on the next start.
Typing `xpack-dotnet-demo` in a terminal starts it too, once the installer's
command folder is on the `PATH` (`~/.local/bin` on macOS and Linux; the
installer says so when it is not).

In a real release the site goes on an HTTPS server, `XPackUpdateUrl` in
`Demo.csproj` points there, and `XPackPreviousPackages` is filled from
wherever earlier releases are kept, such as a GitHub release.

## Remove it

The installation has its own uninstaller:

```sh
# macOS
"$HOME/Library/Application Support/xpack/com.example.dotnetdemo/Uninstall xPack DotNet Demo" --yes
```

On Windows it is in Settings → Apps, like any other application; on Linux,
`~/.local/share/xpack/com.example.dotnetdemo/Uninstall xPack DotNet Demo --yes`.
