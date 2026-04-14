#!/usr/bin/env bash
# bundle_macos.sh — build and package SDRApp.app for macOS distribution
#
# Usage:
#   sh rust/bundle_macos.sh          # from repo root
#   sh bundle_macos.sh               # from rust/
#
# Output: rust/dist/SDRApp.app
#
# Requires: cargo, install_name_tool, codesign, sips, iconutil, otool
set -euo pipefail

# ── Locate the rust/ directory regardless of where the script is invoked ──────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"   # always operate from rust/

BINARY_NAME="sdrapp"
APP_NAME="SDRApp"
BUNDLE_ID="com.sdrapp.sdrapp"
DIST_DIR="dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS/MacOS"
FRAMEWORKS_DIR="$CONTENTS/Frameworks"
RESOURCES_DIR="$CONTENTS/Resources"
ICONSET_DIR="$DIST_DIR/AppIcon.iconset"

ICON_SRC="../upstream-cpp/root/res/icons/sdrpp.macos.png"
INFO_PLIST="macos/Info.plist"
ENTITLEMENTS="macos/Entitlements.plist"

# ── Build ──────────────────────────────────────────────────────────────────────
echo "==> Building release binary..."
cargo build --release

BINARY="target/release/$BINARY_NAME"
if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: binary not found at $BINARY" >&2
    exit 1
fi

# ── Scaffold .app bundle ───────────────────────────────────────────────────────
echo "==> Creating bundle structure..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$FRAMEWORKS_DIR" "$RESOURCES_DIR"

# Copy binary
cp -f "$BINARY" "$MACOS_DIR/$BINARY_NAME"

# Copy Info.plist
cp -f "$INFO_PLIST" "$CONTENTS/Info.plist"

# ── Icon ───────────────────────────────────────────────────────────────────────
if [[ -f "$ICON_SRC" ]]; then
    echo "==> Generating AppIcon.icns..."
    rm -rf "$ICONSET_DIR"
    mkdir -p "$ICONSET_DIR"

    for SIZE in 16 32 64 128 256 512; do
        sips -z "$SIZE" "$SIZE" "$ICON_SRC" --out "$ICONSET_DIR/icon_${SIZE}x${SIZE}.png" > /dev/null 2>&1
        DOUBLE=$((SIZE * 2))
        sips -z "$DOUBLE" "$DOUBLE" "$ICON_SRC" --out "$ICONSET_DIR/icon_${SIZE}x${SIZE}@2x.png" > /dev/null 2>&1
    done

    iconutil --convert icns "$ICONSET_DIR" --output "$RESOURCES_DIR/AppIcon.icns"
    rm -rf "$ICONSET_DIR"
else
    echo "  (icon source not found at $ICON_SRC — skipping icon)"
fi

# ── Bundle dynamic libraries ───────────────────────────────────────────────────
# Collect all non-system dylibs referenced by the binary (and recursively by
# bundled libs) and copy them into Contents/Frameworks/.
#
# System dylibs (in /usr/lib, /System) are left as absolute references because
# macOS guarantees those paths on every machine. Only Homebrew (/usr/local,
# /opt/homebrew) and vendor-installed (/usr/local/lib libsdrplay_api) dylibs
# need to travel with the bundle.
echo "==> Bundling dylibs..."

bundle_dylib() {
    local src_path="$1"
    local lib_name
    lib_name="$(basename "$src_path")"
    local dest="$FRAMEWORKS_DIR/$lib_name"

    # Skip if already bundled
    [[ -f "$dest" ]] && return

    # Skip system libs
    case "$src_path" in
        /usr/lib/*|/System/*|/Library/Apple/*) return ;;
    esac

    if [[ ! -f "$src_path" ]]; then
        echo "  WARNING: dependency not found: $src_path" >&2
        return
    fi

    echo "  bundling $src_path"
    cp -f "$src_path" "$dest"

    # Recurse: collect this lib's own dependencies
    while IFS= read -r dep; do
        dep="${dep%%(*}"    # strip trailing "(compatibility version ...)"
        dep="${dep// /}"    # strip whitespace
        [[ -z "$dep" || "$dep" == "$src_path" ]] && continue
        bundle_dylib "$dep"
    done < <(otool -L "$src_path" 2>/dev/null | tail -n +2 | awk '{print $1}')
}

# Walk all dylibs referenced by the main binary
while IFS= read -r dep; do
    dep="${dep%%(*}"
    dep="${dep// /}"
    [[ -z "$dep" || "$dep" == "$MACOS_DIR/$BINARY_NAME" ]] && continue
    bundle_dylib "$dep"
done < <(otool -L "$MACOS_DIR/$BINARY_NAME" 2>/dev/null | tail -n +2 | awk '{print $1}')

# ── Fix install names ──────────────────────────────────────────────────────────
# Rewrite absolute dylib paths → @rpath/libname.dylib so the bundle is
# self-contained.  The binary gets an LC_RPATH pointing at ../Frameworks.
echo "==> Fixing install names..."

# Add rpath to the binary pointing at the Frameworks dir
if ! otool -l "$MACOS_DIR/$BINARY_NAME" 2>/dev/null | grep -q "@executable_path/../Frameworks"; then
    install_name_tool -add_rpath "@executable_path/../Frameworks" "$MACOS_DIR/$BINARY_NAME"
fi

# Also ensure the binary can find /usr/local/lib for libsdrplay_api (not in Homebrew paths)
if ! otool -l "$MACOS_DIR/$BINARY_NAME" 2>/dev/null | grep -q "/usr/local/lib"; then
    install_name_tool -add_rpath "/usr/local/lib" "$MACOS_DIR/$BINARY_NAME" 2>/dev/null || true
fi

# Rewrite each bundled dylib's id and fix cross-references
shopt -s nullglob
for dylib in "$FRAMEWORKS_DIR"/*.dylib; do
    [[ -f "$dylib" ]] || continue
    lib_name="$(basename "$dylib")"
    old_id="$(otool -D "$dylib" 2>/dev/null | tail -1)"

    # Set the dylib's own install name
    install_name_tool -id "@rpath/$lib_name" "$dylib" 2>/dev/null || true

    # Rewrite the binary's reference to this lib
    install_name_tool -change "$old_id" \
        "@rpath/$lib_name" "$MACOS_DIR/$BINARY_NAME" 2>/dev/null || true

    # Rewrite references in other bundled libs
    for other in "$FRAMEWORKS_DIR"/*.dylib; do
        [[ -f "$other" && "$other" != "$dylib" ]] || continue
        install_name_tool -change "$old_id" \
            "@rpath/$lib_name" "$other" 2>/dev/null || true
    done
done
shopt -u nullglob

# ── Ad-hoc code signature ──────────────────────────────────────────────────────
# Ad-hoc signing (identity "-") allows the app to run on the local machine.
# For distribution, replace "-" with a Developer ID Application certificate.
echo "==> Code-signing (ad-hoc)..."
codesign --force --deep --sign "-" \
    --entitlements "$ENTITLEMENTS" \
    "$APP_DIR" 2>&1 | grep -v "replacing existing signature" || true

# ── Done ───────────────────────────────────────────────────────────────────────
echo ""
echo "Bundle created: $SCRIPT_DIR/$APP_DIR"
echo ""
echo "To run:   open $SCRIPT_DIR/$APP_DIR"
echo "To move:  cp -r $SCRIPT_DIR/$APP_DIR /Applications/"
