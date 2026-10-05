#!/usr/bin/env bash
#
# Installs the package cross-build.sh made for this machine's platform and
# runs it: the proof that a package built on another OS starts here, which
# building it cannot show.
#
# Usage: integrations/dotnet/ci/cross-run.sh XPACK_HOME PACKAGES_DIR PLATFORM
#   XPACK_HOME     folder holding the xpack command line (and its launcher)
#   PACKAGES_DIR   what cross-build.sh wrote
#   PLATFORM       the xPack platform to install, e.g. macos-arm64

set -euo pipefail

xpack_home="$1"
packages="$2"
platform="$3"
xpack="$xpack_home/xpack"
[ -x "$xpack" ] || xpack="$xpack_home/xpack.exe"

package="$(ls "$packages"/*-"$platform".xpkg)"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

"$xpack" verify "$package" --key "$packages/signing.pub.json"
"$xpack" install "$package" --root "$root" --trust "$packages/signing.pub.json"
output="$("$xpack" run com.example.hellocross --root "$root")"
echo "$output"
case "$output" in
    *"hello cross-built for"*) echo "ok: the $platform package built elsewhere runs here" ;;
    *) echo "::error::the $platform package did not print its greeting" >&2; exit 1 ;;
esac
