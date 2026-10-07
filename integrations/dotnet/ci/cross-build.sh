#!/usr/bin/env bash
#
# Publishes a sample application through XPack.Build for several platforms
# from one machine, the way a .NET developer cross-builds: the packages, and
# the public key that verifies them, land in OUT_DIR for cross-run.sh to
# install and run on each platform.
#
# Usage: integrations/dotnet/ci/cross-build.sh XPACK_HOME OUT_DIR RID...
#   XPACK_HOME   folder holding the xpack command line
#   OUT_DIR      where the packages and signing.pub.json are written
#   RID          runtime identifiers to publish, e.g. win-x64 osx-arm64
#   DOTNET       the dotnet to run (default: dotnet on the PATH)

set -euo pipefail

xpack_home="$(cd "$1" && pwd)"
mkdir -p "$2"
out="$(cd "$2" && pwd)"
shift 2
dotnet="${DOTNET:-dotnet}"
here="$(cd "$(dirname "$0")/.." && pwd)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# A version no cache has seen, so the publish below uses this build.
version="0.9.0-ci.$(date +%s)"

"$dotnet" pack "$here/XPack.Build/XPack.Build.csproj" -c Release -o "$work/feed" "-p:Version=$version"

mkdir -p "$work/app"
cat > "$work/app/nuget.config" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="local" value="$work/feed" />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
  </packageSources>
</configuration>
EOF
cat > "$work/app/Hello.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
    <Product>Hello Cross</Product>
    <Version>1.0.0</Version>
    <XPackId>com.example.hellocross</XPackId>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="XPack.Build" Version="$version" PrivateAssets="all" />
  </ItemGroup>
</Project>
EOF
cat > "$work/app/Program.cs" <<'EOF'
System.Console.WriteLine($"hello cross-built for {System.Runtime.InteropServices.RuntimeInformation.RuntimeIdentifier}");
EOF

"$xpack_home/xpack" keygen --out "$work/keys/signing.json" >/dev/null
cp "$work/keys/signing.pub.json" "$out/"

for rid in "$@"; do
    echo "== publishing $rid"
    (cd "$work/app" && "$dotnet" publish -c Release -r "$rid" --self-contained \
        "-p:XPackKey=$work/keys/signing.json" "-p:XPackHome=$xpack_home" \
        "-p:XPackDistDirectory=$out/")
done

ls -la "$out"
