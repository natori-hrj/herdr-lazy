#!/usr/bin/env bash
# Generate documentation media in a disposable workspace.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
candidate=
version=0.45.0
tape=workflow.tape
media=vhs
fixture_args=()
if [[ ${1:-} == --candidate ]]; then
    if [[ ! -f ${2:-} || ! -x ${2:-} || $# -gt 4 ]]; then
        printf 'Usage: %s --candidate BINARY [OUTPUT [FONT_SIZE]]\n' "$0" >&2
        exit 2
    fi
    candidate=$2
    shift 2
    version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$repo/herdr-plugin.toml")
    tape=v0460-workflow.tape
    media=v0460
    fixture_args=(--unicode)
fi
output=${1:-"$repo/docs/media/$media"}
font_size=${2:-22}
case "$font_size" in
    22|24) ;;
    *) printf '%s\n' "Font size must be 22 or 24." >&2; exit 2 ;;
esac
mkdir -p "$output"
output=$(cd "$output" && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work"/{bin,fixture,home,config,data/fonts,cache,state,run}

isolated() {
    env -i PATH="$work/bin:/usr/bin:/bin" HOME="$work/home" XDG_CONFIG_HOME="$work/config" \
        XDG_CACHE_HOME="$work/cache" XDG_DATA_HOME="$work/data" XDG_STATE_HOME="$work/state" \
        TMPDIR="${TMPDIR:-/tmp}" LC_ALL=C.UTF-8 "$@"
}

check_version() {
    local actual
    actual=$(isolated herdr-lazy --version)
    if [[ $actual != "herdr-lazy $version" ]]; then
        printf 'Expected herdr-lazy %s, got %s.\n' "$version" "$actual" >&2
        return 1
    fi
}

if [[ -n $candidate ]]; then
    install -m 755 -- "$candidate" "$work/bin/herdr-lazy"
    check_version
    sha256sum "$work/bin/herdr-lazy"
fi

download() {
    local url=$1 sha=$2 file="$work/${1##*/}"
    if [[ -n ${VHS_ASSET_DIR:-} ]]; then
        cp -- "$VHS_ASSET_DIR/${file##*/}" "$file"
    else
        curl --disable --fail --silent --show-error --location --retry 2 --output "$file" "$url"
    fi
    printf '%s  %s\n' "$sha" "$file" | sha256sum --check >&2
}

vhs=vhs_0.12.1_Linux_x86_64
app=herdr-lazy-0.45.0-c7ddbb421f00-x86_64-unknown-linux-musl
download "https://github.com/charmbracelet/vhs/releases/download/v0.12.1/$vhs.tar.gz" \
    eb33787a5b1cebf91fe0b18bd231e2d71d36c1ff4babd99ddac1a6ab411efd10
download https://github.com/tsl0922/ttyd/releases/download/1.7.7/ttyd.x86_64 \
    8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55
download https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip \
    6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf

tar -xzf "$work/$vhs.tar.gz" -C "$work/bin" --strip-components=1 "$vhs/vhs"
if [[ -z $candidate ]]; then
    download "https://github.com/natori-hrj/herdr-lazy/releases/download/v0.45.0/$app.tar.gz" \
        84b56a215f9b438e9b78b5cf9b24e328a2616ec6e485a365731ce26fc6837df3
    tar -xzf "$work/$app.tar.gz" -C "$work/bin" herdr-lazy
    check_version
fi
install "$work/ttyd.x86_64" "$work/bin/ttyd"
for style in Regular Bold; do
    unzip -p "$work/JetBrainsMono-2.304.zip" "fonts/ttf/JetBrainsMono-$style.ttf" \
        > "$work/data/fonts/JetBrainsMono-$style.ttf"
done

{
    printf '#!/bin/bash\n'
    printf 'exec python3'
    printf ' %q' "$repo/scripts/vhs-launch.py" "$work/bin/herdr-lazy" "$work/fixture" "${fixture_args[@]}"
    printf '\n'
} > "$work/run/launch-demo"
chmod 700 "$work/run/launch-demo"

vhs_version=$(isolated vhs --version)
[[ $vhs_version == 'vhs version v0.12.1 '* ]]
printf '%s\n' "$vhs_version"
[[ $(isolated fc-match -f '%{file}' 'JetBrains Mono') == "$work/data/fonts/JetBrainsMono-Regular.ttf" ]]
(
    cd "$work/run"
    sed "s/^Set FontSize 22$/Set FontSize $font_size/" "$repo/docs/vhs/$tape" > workflow.tape
    isolated vhs workflow.tape
)
[[ $(cat "$work/fixture/config/theme") == nord ]]
assets=(demo.gif demo.mp4 manage.png command-palette.png command-palette-search.png
    marketplace-catalog.png marketplace.png marketplace-refresh.png details.png
    theme-catppuccin.png theme-selection.png manage-themed.png)
if [[ -n $candidate ]]; then
    assets+=(help.png discovery-filtered.png discovery-reset.png discovery-empty.png)
fi
for asset in "${assets[@]}"; do
    test -s "$work/run/$asset"
done
for asset in "${assets[@]}"; do
    install -m 644 "$work/run/$asset" "$output/$asset"
done
