#!/bin/sh
# Build artifacts must not compete with the installed app in Launch Services.
set -eu
app=${1:?Usage: macos-bundle-identity.sh APP debug|release|installed}
kind=${2:?Missing bundle kind}
case "$kind" in
    debug) identifier=de.dme86.rst.debug; name='rst Debug' ;;
    release) identifier=de.dme86.rst.preview; name='rst Preview' ;;
    installed) identifier=de.dme86.rst; name=rst ;;
    *) echo "rst: unknown bundle kind: $kind" >&2; exit 1 ;;
esac
plist="$app/Contents/Info.plist"
test -f "$plist"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
# Remove the old identity for this exact path before updating its metadata.
if [ "${RST_REGISTER_APP:-1}" != 0 ] && [ -x "$lsregister" ]; then "$lsregister" -u "$app" >/dev/null 2>&1 || true; fi
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $identifier" "$plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $name" "$plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $name" "$plist"
# Bind the final plist/resources to the local ad-hoc signature. This is not a
# Developer ID signature or notarization; no signing identity is required.
/usr/bin/codesign --force --sign - --identifier "$identifier" "$app"
touch "$app"
if [ "${RST_REGISTER_APP:-1}" != 0 ] && [ -x "$lsregister" ]; then "$lsregister" -f "$app" >/dev/null 2>&1 || true; fi
