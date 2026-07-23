#!/bin/sh
# Cut a release: bump the version, commit, tag, push.
#
#   scripts/release.sh 0.12.0
#   scripts/release.sh 0.12.0 --dry-run
#
# The steps have been the same every time, and two of them are easy to get half-right in ways
# that are not obvious afterwards:
#
#   - the version lives in BOTH herdr-plugin.toml (which herdr reads, and which names the
#     release assets) and Cargo.toml (which the build fingerprint is computed from). Bumping
#     one without the other publishes assets nobody will look for.
#   - bumping the version without pushing the tag leaves every install compiling from source,
#     silently, because the prebuilt binary for that fingerprint does not exist yet.
#
# So this refuses to start unless the tree is in a state where the whole sequence can finish.

set -eu

usage() {
    echo "usage: scripts/release.sh <version> [--dry-run]" >&2
    echo "   e.g. scripts/release.sh 0.12.0" >&2
    exit 2
}

VERSION="${1:-}"
[ -n "$VERSION" ] || usage
DRY_RUN=""
[ "${2:-}" = "--dry-run" ] && DRY_RUN=1

cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }
step() { echo "→ $*"; }
run() {
    if [ -n "$DRY_RUN" ]; then
        echo "   would run: $*"
    else
        "$@"
    fi
}

# --- checks, before touching anything -------------------------------------------------

case "$VERSION" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) die "version must look like 1.2.3, got '$VERSION'" ;;
esac

CURRENT=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' herdr-plugin.toml | head -1)
[ -n "$CURRENT" ] || die "cannot read the current version from herdr-plugin.toml"
[ "$CURRENT" != "$VERSION" ] || die "already at $VERSION"

# Compare as numbers, not strings: "0.9.0" > "0.10.0" lexically, and releasing backwards
# would publish assets that older installs then prefer.
newer=$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)
[ "$newer" = "$VERSION" ] || die "$VERSION is not newer than the current $CURRENT"

git rev-parse --git-dir >/dev/null 2>&1 || die "not a git repository"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
[ "$BRANCH" = "main" ] || die "on branch '$BRANCH' — releases are cut from main"

[ -z "$(git status --porcelain)" ] || die "working tree is dirty; commit or stash first"

git fetch --quiet origin main 2>/dev/null || true
if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main 2>/dev/null || echo none)" ]; then
    die "main and origin/main differ; pull or push first"
fi

git rev-parse "v$VERSION" >/dev/null 2>&1 && die "tag v$VERSION already exists"
if command -v gh >/dev/null 2>&1; then
    gh release view "v$VERSION" >/dev/null 2>&1 && die "release v$VERSION already exists"
fi

echo "release $CURRENT -> $VERSION"
[ -n "$DRY_RUN" ] && echo "(dry run: nothing will be changed)"
echo

# --- the release ----------------------------------------------------------------------

step "checking the build is green first"
run cargo fmt --check
run cargo clippy --all-targets -- -D warnings
run cargo test --locked

step "bumping the version in herdr-plugin.toml and Cargo.toml"
if [ -z "$DRY_RUN" ]; then
    for f in herdr-plugin.toml Cargo.toml; do
        # Only the first `version =` line, which is the package's own; a dependency further
        # down the file must not be rewritten.
        awk -v v="$VERSION" '
            !done && /^version *= *"/ { sub(/"[^"]*"/, "\"" v "\""); done = 1 }
            { print }
        ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    done
    for f in herdr-plugin.toml Cargo.toml; do
        got=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$f" | head -1)
        [ "$got" = "$VERSION" ] || die "$f still says $got — bump failed, nothing committed"
    done
fi

step "rebuilding so Cargo.lock records the new version"
run cargo build --release

if [ -z "$DRY_RUN" ]; then
    FINGERPRINT=$(sh scripts/build-fingerprint.sh)
    echo "   assets will be herdr-lazy-$VERSION-$FINGERPRINT-<target>.tar.gz"
fi

step "committing"
run git add -A
run git commit -m "chore: release $VERSION"

step "tagging and pushing"
run git tag -a "v$VERSION" -m "v$VERSION"
run git push origin main
run git push origin "v$VERSION"

echo
if [ -n "$DRY_RUN" ]; then
    echo "dry run finished — nothing was changed."
    exit 0
fi

cat <<EOF
pushed. The release workflow is building now.

  gh run watch \$(gh run list --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')
  gh release view v$VERSION --json isDraft,assets -q '"draft: \(.isDraft) | assets: \(.assets|length)"'

Expect draft: false and 8 assets. The workflow verifies each asset is actually downloadable
at its published URL, so a green run means installs will find it.
EOF
