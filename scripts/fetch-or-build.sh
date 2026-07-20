#!/bin/sh
# Produce target/release/herdr-lazy, preferring a prebuilt binary from GitHub Releases.
#
# herdr runs this as the plugin's [[build]] step, with the plugin directory as cwd.
# Installing a plugin should not require a Rust toolchain, so try a signed-by-checksum
# download first; fall back to `cargo build --release` when anything at all goes wrong
# (no release for this version, unknown platform, no network, checksum mismatch).
#
# The fallback is not an error path — it is the guaranteed path. A failed download must
# never be worse than not having tried, so every failure here is non-fatal until the very
# end, where cargo either succeeds or the install legitimately fails.

set -eu

REPO="natori-hrj/herdr-lazy"
OUT_DIR="target/release"
OUT="$OUT_DIR/herdr-lazy"

log() { printf '[herdr-lazy build] %s\n' "$*" >&2; }

# Locate cargo, which is usually NOT on the PATH herdr builds with.
#
# herdr spawns build commands with a minimal PATH (observed: /usr/gnu/bin:/usr/local/bin:
# /bin:/usr/bin:.) that does not include ~/.cargo/bin, so a bare `cargo build` fails with a
# bare "No such file or directory" even on machines where Rust is installed and working in
# the user's shell. Look in the standard install locations before concluding anything.
find_cargo() {
    if command -v cargo >/dev/null 2>&1; then
        command -v cargo
        return 0
    fi
    for candidate in \
        "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" \
        "$HOME/.cargo/bin/cargo" \
        /usr/local/cargo/bin/cargo \
        /opt/homebrew/bin/cargo \
        /usr/local/bin/cargo
    do
        [ -x "$candidate" ] && { printf '%s\n' "$candidate"; return 0; }
    done
    return 1
}

build_from_source() {
    log "building from source"
    cargo_bin=$(find_cargo || true)
    if [ -z "$cargo_bin" ]; then
        log "ERROR: no prebuilt binary was usable, and cargo could not be found."
        log "Looked on PATH ($PATH) and in the usual install locations."
        log "If Rust is installed somewhere unusual, set CARGO_HOME and reinstall."
        log "Otherwise install Rust (https://rustup.rs) and reinstall this plugin."
        exit 1
    fi
    log "using cargo at $cargo_bin"
    "$cargo_bin" build --release
    exit 0
}

# Version comes from the manifest, which is the single source of truth herdr also reads.
VERSION=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' herdr-plugin.toml | head -1)
[ -n "$VERSION" ] || build_from_source

case "$(uname -s)" in
    Darwin) OS="apple-darwin" ;;
    Linux)  OS="unknown-linux-gnu" ;;
    *)      log "unrecognised OS $(uname -s)"; build_from_source ;;
esac

case "$(uname -m)" in
    arm64|aarch64) ARCH="aarch64" ;;
    x86_64|amd64)  ARCH="x86_64" ;;
    *)             log "unrecognised architecture $(uname -m)"; build_from_source ;;
esac

TRIPLE="$ARCH-$OS"

# The asset name carries a fingerprint of the source this binary was built from. herdr
# installs the default branch HEAD, which routinely runs ahead of the last release — without
# this, the version alone would happily match a stale binary against newer source, and the
# user would run behaviour that does not correspond to the code they have. When the source
# has moved, the name simply does not exist and we compile instead.
FINGERPRINT=$(sh scripts/build-fingerprint.sh 2>/dev/null || true)
if [ -z "$FINGERPRINT" ]; then
    log "could not fingerprint the source"
    build_from_source
fi

ASSET="herdr-lazy-$VERSION-$FINGERPRINT-$TRIPLE.tar.gz"
# Overridable so the download path itself can be exercised in a test, instead of only ever
# being proven by the fallback firing.
BASE="${HERDR_LAZY_RELEASE_BASE:-https://github.com/$REPO/releases/download/v$VERSION}"

fetch() {
    # $1 url, $2 destination. Quiet, fail on HTTP errors, follow redirects.
    if command -v curl >/dev/null 2>&1; then
        curl -fsL --retry 2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        return 1
    fi
}

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        return 1
    fi
}

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t herdr-lazy)
# shellcheck disable=SC2064  # expand TMP now: it must be removed even if the var changes.
trap "rm -rf '$TMP'" EXIT INT TERM

log "looking for a prebuilt $TRIPLE binary for v$VERSION ($FINGERPRINT)"
if ! fetch "$BASE/$ASSET" "$TMP/$ASSET"; then
    log "no prebuilt binary matching this source ($FINGERPRINT) — the release is older"
    build_from_source
fi

# A checksum is mandatory: an unverified download is worse than compiling.
if ! fetch "$BASE/$ASSET.sha256" "$TMP/$ASSET.sha256"; then
    log "no checksum published for $ASSET — refusing to trust the download"
    build_from_source
fi

WANT=$(cut -d' ' -f1 <"$TMP/$ASSET.sha256")
GOT=$(sha256_of "$TMP/$ASSET" || true)
if [ -z "$GOT" ]; then
    log "no sha256 tool available to verify the download"
    build_from_source
fi
if [ "$WANT" != "$GOT" ]; then
    log "CHECKSUM MISMATCH for $ASSET (expected $WANT, got $GOT)"
    build_from_source
fi

if ! tar -xzf "$TMP/$ASSET" -C "$TMP" 2>/dev/null; then
    log "could not extract $ASSET"
    build_from_source
fi
[ -f "$TMP/herdr-lazy" ] || { log "archive did not contain herdr-lazy"; build_from_source; }

mkdir -p "$OUT_DIR"
# Install via a temporary name and rename, so a partial write never leaves a broken binary
# at the path herdr is about to execute.
cp "$TMP/herdr-lazy" "$OUT.new"
chmod +x "$OUT.new"
mv "$OUT.new" "$OUT"
log "installed prebuilt binary (sha256 $GOT)"
