#!/bin/sh

set -eu

usage() {
    echo "usage: scripts/release.sh <version> [--dry-run]" >&2
    exit 2
}

VERSION="${1:-}"
[ -n "$VERSION" ] || usage
DRY_RUN=""
case "$#:${2:-}" in
    1:) ;;
    2:--dry-run) DRY_RUN=1 ;;
    *) usage ;;
esac

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

case "$VERSION" in
    *[!0-9.]*) die "version must look like 1.2.3, got '$VERSION'" ;;
esac
printf '%s\n' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' ||
    die "version must look like 1.2.3, got '$VERSION'"

CURRENT=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' herdr-plugin.toml | head -1)
[ -n "$CURRENT" ] || die "cannot read the current version from herdr-plugin.toml"
[ "$CURRENT" != "$VERSION" ] || die "already at $VERSION"

newer=$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)
[ "$newer" = "$VERSION" ] || die "$VERSION is not newer than the current $CURRENT"

git rev-parse --git-dir >/dev/null 2>&1 || die "not a git repository"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
[ "$BRANCH" = "main" ] || die "on branch '$BRANCH' — releases are cut from main"

[ -z "$(git status --porcelain)" ] || die "working tree is dirty; commit or stash first"

TAG="v$VERSION"
remote_refs=$(git ls-remote --exit-code origin refs/heads/main "refs/tags/$TAG") ||
    die "cannot verify main and $TAG on origin"
remote_main=$(printf '%s\n' "$remote_refs" | awk '$2 == "refs/heads/main" { print $1 }')
[ "$(git rev-parse HEAD)" = "$remote_main" ] || die "main and origin/main differ; pull or push first"

if git show-ref --verify --quiet "refs/tags/$TAG" ||
    printf '%s\n' "$remote_refs" | awk -v tag="refs/tags/$TAG" '$2 == tag { found = 1 } END { exit !found }'; then
    die "tag $TAG already exists"
fi

previous_tag="v$CURRENT"
git merge-base --is-ancestor "refs/tags/$previous_tag" HEAD ||
    die "release tag $previous_tag is missing or outside main history; check your local tags"

echo "release $CURRENT -> $VERSION"
[ -n "$DRY_RUN" ] && echo "(dry run)"
step "changes since $previous_tag"
git --no-pager log --first-parent --oneline "$previous_tag..HEAD"
echo

step "checking the build is green first"
run cargo fmt --check
run cargo check --locked --all-targets
run cargo clippy --locked --all-targets -- -D warnings
run cargo test --locked

step "bumping the version in herdr-plugin.toml and Cargo.toml"
if [ -z "$DRY_RUN" ]; then
    for f in herdr-plugin.toml Cargo.toml; do
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
run git add -- Cargo.toml Cargo.lock herdr-plugin.toml
run git commit -m "chore: release $VERSION"

step "tagging and pushing"
run git tag -a "$TAG" -m "$TAG"
run git push --atomic origin main "refs/tags/$TAG"

echo
if [ -n "$DRY_RUN" ]; then
    echo "dry run finished — nothing was changed."
    exit 0
fi

cat <<EOF
pushed. The release workflow is building now.

  gh run watch \$(gh run list --workflow=release.yml --branch=$TAG --limit 1 --json databaseId -q '.[0].databaseId')
  gh release view v$VERSION --json isDraft,assets -q '"draft: \(.isDraft) | assets: \(.assets|length)"'

The release is published after all five archives and their checksums are ready.
EOF
