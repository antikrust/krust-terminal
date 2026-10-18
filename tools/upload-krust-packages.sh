#!/usr/bin/env bash
# Rsync built Krust installers to the download site. Mirrors bob3's
# upload-bob3-packages.sh. Called by the linux lane of .forgejo/workflows/release.yml
# (skipped for -test tags). Override the remote/path via env if the site moves.
#
# download.html links to STABLE, version-free filenames (krust_amd64.deb, not
# krust-terminal_0.1.0_amd64.deb), so we stage renamed copies before rsyncing
# rather than uploading the bundler's own versioned names verbatim.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ssh alias + docroot for the download site. Default targets the derp.baby site;
# the runner's ssh config / keys must be able to reach it (same key setup bob3 uses).
WEB_REMOTE="${KRUST_WEB_REMOTE:-therob:/var/www/derp.baby}"
WEB_PATH="${KRUST_WEB_PATH:-downloads}"

shopt -s nullglob
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

stage() { # stage <src> <stable-name>
    [ -f "$1" ] || return 0
    # Two packages mapping to one stable name means a stale build is lying
    # around (e.g. an old bork-terminal_*.deb next to the new krust one) --
    # refuse rather than let whichever was copied last silently win.
    if [ -e "$STAGE/$2" ]; then
        echo "error: more than one package maps to $2 (second: $(basename "$1")) -- refusing to guess" >&2
        exit 1
    fi
    cp "$1" "$STAGE/$2"
}

# --- macOS ---
for f in "$ROOT"/dist/macos/*.dmg; do stage "$f" "Krust.dmg"; done

# --- Linux: arch is encoded in the bundler's own filename ---
for f in "$ROOT"/dist/linux/*.deb; do
    case "$f" in
        *arm64*|*aarch64*) stage "$f" "krust_arm64.deb" ;;
        *)                 stage "$f" "krust_amd64.deb" ;;
    esac
done
for f in "$ROOT"/dist/linux/*.rpm; do
    case "$f" in
        *aarch64*|*arm64*) stage "$f" "krust.aarch64.rpm" ;;
        *)                 stage "$f" "krust.x86_64.rpm" ;;
    esac
done
for f in "$ROOT"/dist/linux/*.AppImage; do
    case "$f" in
        *aarch64*|*arm64*) stage "$f" "Krust-aarch64.AppImage" ;;
        *)                 stage "$f" "Krust-x86_64.AppImage" ;;
    esac
done
for f in "$ROOT"/dist/linux/*.pkg.tar.zst; do
    case "$f" in
        *aarch64*|*arm64*) stage "$f" "krust-aarch64.pkg.tar.zst" ;;
        *)                 stage "$f" "krust-x86_64.pkg.tar.zst" ;;
    esac
done

# --- Windows ---
for f in "$ROOT"/dist/windows/*.exe "$ROOT"/dist/windows/*.msi; do
    case "$f" in
        *arm64*|*aarch64*) stage "$f" "KrustSetup-arm64.exe" ;;
        *)                 stage "$f" "KrustSetup-x64.exe" ;;
    esac
done

staged=("$STAGE"/*)
if (( ${#staged[@]} == 0 )); then
    echo "No Krust packages found under dist/ -- nothing to upload."
    exit 1
fi

printf 'uploading %d file(s) to %s/%s/\n' "${#staged[@]}" "$WEB_REMOTE" "$WEB_PATH"
for f in "${staged[@]}"; do echo "  $(basename "$f")"; done
rsync -av "${staged[@]}" "$WEB_REMOTE/$WEB_PATH/"
