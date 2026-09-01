#!/bin/sh
# Copycat installer.
#
#   curl -fsSL https://raw.githubusercontent.com/samishal1998/copycat/main/install.sh | sh
#
# Installs two binaries: `copycat` (the CLI, which contains the TUI as
# `copycat tui`) and `copycatd` (the daemon). Nothing works without the daemon,
# so they always install together.
#
# Environment:
#   COPYCAT_VERSION   tag to install, e.g. v0.1.0   (default: latest release)
#   COPYCAT_BIN_DIR   install directory             (default: ~/.local/bin)
#
# On verification: this checks the download against the SHA256SUMS published
# with the same release. That catches a truncated or corrupted transfer. It is
# not a signature — it does not tell you the release itself is trustworthy, and
# no installer that fetches both the archive and its checksum from one place
# can claim otherwise. Read the script before piping it to a shell.

set -eu

REPO="samishal1998/copycat"
BIN_DIR="${COPYCAT_BIN_DIR:-$HOME/.local/bin}"
TMP=""

say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
die()  { printf '\nerror: %s\n' "$*" >&2; exit 1; }

cleanup() { [ -n "$TMP" ] && rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs \`$1\`, which is not on your PATH"
}

# curl or wget, whichever exists.
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        die "neither curl nor wget is available"
    fi
}

fetch_stdout() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$1"
    else
        die "neither curl nor wget is available"
    fi
}

detect_target() {
    kernel=$(uname -s)
    machine=$(uname -m)

    case "$kernel" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*)
            die "run the Windows installer instead: download the .zip from
       https://github.com/$REPO/releases and put copycat.exe and copycatd.exe on your PATH" ;;
        *) die "unsupported operating system: $kernel" ;;
    esac

    case "$machine" in
        x86_64|amd64)  arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *) die "unsupported architecture: $machine
       Only x86_64 and aarch64 are built. Build from source with \`cargo install --path crates/copycat-cli\`." ;;
    esac

    printf '%s-%s' "$arch" "$os"
}

latest_version() {
    # The redirect from /releases/latest carries the tag, which avoids needing
    # a JSON parser or an API token just to learn a version number.
    if command -v curl >/dev/null 2>&1; then
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
            || die "cannot reach GitHub to find the latest release"
    else
        url=$(wget -qS --max-redirect=10 -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 \
              | awk '/^  Location: /{print $2}' | tail -1) \
            || die "cannot reach GitHub to find the latest release"
    fi
    version=${url##*/}
    case "$version" in
        v*) printf '%s' "$version" ;;
        *)  die "could not determine the latest version (got \"$version\").
       Set COPYCAT_VERSION to a tag, or check https://github.com/$REPO/releases" ;;
    esac
}

# verify <path-to-file> <path-to-SHA256SUMS> <name-as-listed>
#
# Deliberately not run in a subshell: `die` there would only exit the subshell,
# and a failed checksum must stop the install, not merely be noted.
verify() {
    expected=$(awk -v f="$3" '$2 == f || $2 == "*" f { print $1 }' "$2" | head -1)
    [ -n "$expected" ] || die "$3 is not listed in SHA256SUMS"

    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$1" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$1" | awk '{print $1}')
    else
        die "no sha256sum or shasum available to verify the download"
    fi

    [ "$actual" = "$expected" ] || die "checksum mismatch for $3
       expected $expected
       got      $actual
       Do not use this download."
    info "checksum ok"
}

main() {
    need uname
    need tar
    need awk

    target=$(detect_target)
    version="${COPYCAT_VERSION:-$(latest_version)}"
    archive="copycat-${version}-${target}.tar.gz"
    base="https://github.com/$REPO/releases/download/$version"

    say ""
    say "copycat $version"
    info "target    $target"
    info "install   $BIN_DIR"
    say ""

    TMP=$(mktemp -d 2>/dev/null || mktemp -d -t copycat)
    [ -d "$TMP" ] || die "could not create a temporary directory"

    info "downloading $archive"
    fetch "$base/$archive" "$TMP/$archive" \
        || die "no build for $target in $version
       Available archives: https://github.com/$REPO/releases/tag/$version"

    fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS" || die "could not download SHA256SUMS"
    verify "$TMP/$archive" "$TMP/SHA256SUMS" "$archive"

    tar -xzf "$TMP/$archive" -C "$TMP" || die "could not extract $archive"

    mkdir -p "$BIN_DIR" || die "could not create $BIN_DIR"
    for binary in copycat copycatd; do
        [ -f "$TMP/$binary" ] || die "$binary is missing from the archive"
        # Install to a temporary name and rename, so a running daemon is
        # replaced atomically rather than truncated mid-write.
        cp "$TMP/$binary" "$BIN_DIR/.$binary.new" || die "could not write to $BIN_DIR"
        chmod 755 "$BIN_DIR/.$binary.new"
        mv "$BIN_DIR/.$binary.new" "$BIN_DIR/$binary" || die "could not install $binary"
        info "installed $BIN_DIR/$binary"
    done

    say ""
    case ":$PATH:" in
        *":$BIN_DIR:"*)
            say "Next:"
            say "  copycat daemon start"
            say "  copycat doctor        # what works on this machine, and what does not"
            say "  copycat tui"
            ;;
        *)
            say "$BIN_DIR is not on your PATH. Add it:"
            say ""
            say "  export PATH=\"\$PATH:$BIN_DIR\""
            say ""
            say "then:  copycat daemon start && copycat doctor"
            ;;
    esac
    say ""
}

main "$@"
