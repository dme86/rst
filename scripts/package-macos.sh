#!/bin/sh
set -eu
if [ "$(uname -s)" != "Darwin" ]; then
  echo "rst: packaging requires macOS (sips and iconutil)" >&2
  exit 1
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=${1:-debug}
case "$profile" in
  debug|release) ;;
  *) echo "Usage: $0 [debug|release]" >&2; exit 2 ;;
esac
binary="$root/target/$profile/rst"
case "$profile" in debug) name='rst Debug';; release) name='rst Preview';; esac
app="$root/target/$profile/$name.app"
icon="$root/assets/logo.png"
if [ ! -f "$binary" ] || [ ! -f "$icon" ]; then
  echo "rst: missing built binary or assets/logo.png" >&2
  exit 1
fi
sh "$root/scripts/migrate-macos-bundles.sh" "$root"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/rst"
chmod 755 "$app/Contents/MacOS/rst"
cp "$root/packaging/macos/Info.plist" "$app/Contents/Info.plist"
temp_root=${TMPDIR:-/tmp}
temporary=$(mktemp -d "$temp_root/rst-icon.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
iconset="$temporary/rst.iconset"
mkdir -p "$iconset"
for entry in '16 icon_16x16' '32 icon_16x16@2x' '32 icon_32x32' \
             '64 icon_32x32@2x' '128 icon_128x128' '256 icon_128x128@2x' \
             '256 icon_256x256' '512 icon_256x256@2x' '512 icon_512x512' \
             '1024 icon_512x512@2x'; do
  set -- $entry
  sips -z "$1" "$1" "$icon" --out "$iconset/$2.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/rst.icns"
# Preserve the exact README icon in the bundle as well. The application
# also embeds it at compile time for its live Dock/Cmd-Tab icon.
cp "$icon" "$app/Contents/Resources/logo.png"
# Give previews/debug builds a distinct identity before registering them.
sh "$root/scripts/macos-bundle-identity.sh" "$app" "$profile"
echo "rst: packaged $app with assets/logo.png"
