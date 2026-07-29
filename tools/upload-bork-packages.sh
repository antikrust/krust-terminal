#!/usr/bin/env bash
# Rsync built Bork installers to the download site. Mirrors bob3's
# upload-bob3-packages.sh. Called by the linux lane of .forgejo/workflows/release.yml
# (skipped for -test tags). Override the remote/path via env if the site moves.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ssh alias + docroot for the download site. Default targets the derp.baby site;
# the runner's ssh config / keys must be able to reach it (same key setup bob3 uses).
WEB_REMOTE="${BORK_WEB_REMOTE:-therob:/var/www/derp.baby}"
WEB_PATH="${BORK_WEB_PATH:-downloads}"

shopt -s nullglob
packages=(
    "$ROOT"/dist/macos/*.dmg
    "$ROOT"/dist/macos/*.pkg
    "$ROOT"/dist/linux/*.AppImage
    "$ROOT"/dist/linux/*.deb
    "$ROOT"/dist/linux/*.rpm
    "$ROOT"/dist/linux/*.pkg.tar.zst
    "$ROOT"/dist/windows/*.exe
    "$ROOT"/dist/windows/*.msi
    "$ROOT"/dist/windows/*.zip
)

if (( ${#packages[@]} == 0 )); then
    echo "No Bork packages found under dist/ -- nothing to upload."
    exit 1
fi

printf 'uploading %d file(s) to %s/%s/\n' "${#packages[@]}" "$WEB_REMOTE" "$WEB_PATH"
rsync -av "${packages[@]}" "$WEB_REMOTE/$WEB_PATH/"
