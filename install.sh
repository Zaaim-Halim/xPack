#!/bin/sh
#
# Installs the xPack command line and the runtime binaries that ship with it.
#
#   curl -fsSL https://raw.githubusercontent.com/Zaaim-Halim/xPack/main/install.sh | sh
#
# Reads these from the environment:
#
#   XPACK_VERSION       version to install, e.g. 0.2.0. Default: the latest release
#   XPACK_INSTALL_DIR   where to put the binaries. Default: ~/.local/xpack
#   XPACK_REPO          owner/name to install from. Default: Zaaim-Halim/xPack
#
# Everything is wrapped in a function that is called on the last line. A script
# piped into a shell runs as it arrives, so a download cut halfway through
# would otherwise execute the half that got there.

set -eu

REPO="${XPACK_REPO:-Zaaim-Halim/xPack}"

# Every binary an installation needs. They go in one directory and stay
# together: the CLI finds the launcher, updater and uninstaller beside itself,
# so a stray `xpack` on its own packs fine and then fails to install.
BINARIES="xpack xpack-launcher xpack-updater xpack-uninstaller xpack-installer"

say() {
    printf '%s\n' "$*"
}

die() {
    printf 'install: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1
}

# Names the platform the way the release archives do.
detect_platform() {
    _os=$(uname -s)
    _arch=$(uname -m)

    case "$_os" in
        Linux) _os=linux ;;
        Darwin) _os=macos ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "this script does not install on Windows.
Download the windows-x64 zip from https://github.com/$REPO/releases,
unpack it, and add the directory to your PATH."
            ;;
        *) die "unsupported operating system: $_os" ;;
    esac

    case "$_arch" in
        x86_64 | amd64) _arch=x64 ;;
        aarch64 | arm64) _arch=arm64 ;;
        *) die "unsupported architecture: $_arch" ;;
    esac

    printf '%s-%s' "$_os" "$_arch"
}

# Fetches a URL to a file. curl where there is one, wget otherwise.
# Variables inside these functions carry an underscore because a POSIX shell
# has no local scope: a plain `name=$1` here would overwrite the caller's.
fetch() {
    _url=$1
    _destination=$2
    if need curl; then
        curl -fsSL --retry 3 --retry-delay 2 -o "$_destination" "$_url" \
            || die "could not download $_url"
    elif need wget; then
        wget -q --tries=3 -O "$_destination" "$_url" \
            || die "could not download $_url"
    else
        die "neither curl nor wget is installed"
    fi
}

# Reads a URL to standard output.
fetch_stdout() {
    _url=$1
    if need curl; then
        curl -fsSL --retry 3 --retry-delay 2 "$_url"
    elif need wget; then
        wget -qO- --tries=3 "$_url"
    else
        die "neither curl nor wget is installed"
    fi
}

# The tag of the most recent release, without the leading v.
#
# Asked of the API rather than guessed from the `latest` redirect, because the
# archive's name carries its version and the redirect cannot be followed
# without already knowing it.
latest_version() {
    _body=$(fetch_stdout "https://api.github.com/repos/$REPO/releases/latest") \
        || die "could not ask GitHub for the latest release"
    _tag=$(printf '%s' "$_body" \
        | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' \
        | head -n1 \
        | sed 's/.*"\([^"]*\)"$/\1/')
    [ -n "$_tag" ] || die "no releases found at https://github.com/$REPO/releases"
    printf '%s' "${_tag#v}"
}

# Checks the archive against the published SHA256SUMS.
#
# Worth doing even over HTTPS: it is the difference between trusting the
# transport and checking that the bytes are the ones the release lists.
verify() {
    _archive=$1
    _sums=$2
    _name=$3

    _expected=$(grep "  $_name\$" "$_sums" | head -n1 | cut -d' ' -f1)
    [ -n "$_expected" ] || die "$_name is not listed in SHA256SUMS"

    if need sha256sum; then
        _actual=$(sha256sum "$_archive" | cut -d' ' -f1)
    elif need shasum; then
        _actual=$(shasum -a 256 "$_archive" | cut -d' ' -f1)
    else
        say "warning: no sha256sum or shasum, so the download was not verified"
        return 0
    fi

    [ "$_actual" = "$_expected" ] || die "$_name failed its checksum.
  expected $_expected
  got      $_actual"
    say "checksum ok"
}

main() {
    platform=$(detect_platform)
    version="${XPACK_VERSION:-}"
    if [ -z "$version" ]; then
        say "looking up the latest release"
        version=$(latest_version)
    fi

    install_dir="${XPACK_INSTALL_DIR:-$HOME/.local/xpack}"
    name="xpack-$version-$platform"
    archive="$name.tar.gz"
    base="https://github.com/$REPO/releases/download/v$version"

    say "installing xPack $version for $platform"

    need tar || die "tar is not installed"

    work=$(mktemp -d 2>/dev/null || mktemp -d -t xpack)
    # The temporary directory goes whether this succeeds or fails, including
    # on an interrupt, so a cancelled install leaves nothing behind.
    trap 'rm -rf "$work"' EXIT INT TERM

    say "downloading $archive"
    fetch "$base/$archive" "$work/$archive"
    fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
    verify "$work/$archive" "$work/SHA256SUMS" "$archive"

    # Unpacked from inside the directory rather than with -C, because the two
    # tars in circulation disagree about where -C belongs relative to the
    # archive and one of them silently resolves the archive against it.
    (cd "$work" && tar xzf "$archive") || die "could not unpack $archive"
    [ -d "$work/$name" ] || die "$archive did not contain $name"

    # Replaced rather than merged. Leaving an old binary beside a new one is
    # how an installation ends up running two versions of xPack at once.
    mkdir -p "$(dirname "$install_dir")"
    rm -rf "$install_dir"
    mv "$work/$name" "$install_dir"

    for binary in $BINARIES; do
        [ -f "$install_dir/$binary" ] || die "$binary is missing from the archive"
        chmod +x "$install_dir/$binary"
    done

    installed=$("$install_dir/xpack" --version 2>/dev/null) \
        || die "the installed xpack does not run"

    say ""
    say "installed $installed"
    say "  into $install_dir"

    case ":${PATH}:" in
        *":$install_dir:"*)
            say ""
            say "Already on your PATH. Try: xpack --help"
            ;;
        *)
            say ""
            say "Add it to your PATH:"
            say ""
            say "  export PATH=\"\$PATH:$install_dir\""
            say ""
            say "Put that in ~/.profile, ~/.bashrc or ~/.zshrc to keep it."
            ;;
    esac
}

main "$@"
