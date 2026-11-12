#!/bin/sh
# One-time migration of generated bundles that previously shared rst's name/ID.
set -eu
root=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
for profile in debug release; do
    legacy="$root/target/$profile/rst.app"
    case "$profile" in debug) name='rst Debug';; release) name='rst Preview';; esac
    named="$root/target/$profile/$name.app"
    [ -d "$legacy" ] || continue
    identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$legacy/Contents/Info.plist")
    case "$identifier" in de.dme86.rst|de.dme86.rst.debug|de.dme86.rst.preview) ;; *)
        echo "rst: refusing to migrate unrelated app: $legacy" >&2; exit 1;; esac
    if [ "${RST_REGISTER_APP:-1}" != 0 ] && [ -x "$lsregister" ]; then "$lsregister" -u "$legacy" >/dev/null 2>&1 || true; fi
    if [ -d "$named" ]; then
        # The named build already supersedes this generated legacy duplicate.
        rm -rf "$legacy"
    else
        mv "$legacy" "$named"
        sh "$root/scripts/macos-bundle-identity.sh" "$named" "$profile"
    fi
done
