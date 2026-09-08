#!/bin/sh
# Print a short, deterministic hash of everything that affects the compiled binary.
#
# Used by the Unix release workflow and install script. `build-fingerprint.ps1` mirrors this
# byte-for-byte for Windows; CI compares both outputs. If they ever differ, the install falls
# back to a local build instead of silently fetching a binary built from different source.
#
# Why a fingerprint of the source rather than the git commit: herdr installs the default
# branch HEAD, so a README or CI commit moves the commit id without changing the binary at
# all. Keying on the commit would force everyone to compile after every docs typo. Keying on
# the build inputs means a prebuilt binary is reused exactly when it is still correct.
#
# Failure direction is deliberate: if this ever disagrees between the two callers, the asset
# name will not match, the download 404s, and the install compiles from source. Slower, never
# wrong.

set -eu

cd "$(dirname "$0")/.."

sha256_stdin() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | cut -d' ' -f1
    else
        echo "no sha256 tool available" >&2
        return 1
    fi
}

# Sort with a fixed collation so the order cannot vary by locale. Paths are relative to the
# repository root, so an absolute path never enters the hash.
{
    for f in Cargo.toml Cargo.lock; do
        [ -f "$f" ] && printf '%s\n' "$f" && cat "$f"
    done
    find src -type f -name '*.rs' | LC_ALL=C sort | while IFS= read -r f; do
        printf '%s\n' "$f"
        cat "$f"
    done
} | sha256_stdin | cut -c1-12
