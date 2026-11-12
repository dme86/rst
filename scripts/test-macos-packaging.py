#!/usr/bin/env python3
"""Check packaging without registering fixtures in Launch Services."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

if sys.platform != 'darwin':
    raise SystemExit('macOS bundle tests require macOS')
root = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='rst-packaging-') as temporary:
    app = Path(temporary) / 'rst.app'
    contents = app / 'Contents'
    (contents / 'MacOS').mkdir(parents=True)
    shutil.copyfile(root / 'packaging/macos/Info.plist', contents / 'Info.plist')
    shutil.copy2(root / 'target/release/rst', contents / 'MacOS/rst')
    identifiers = set()
    # Exercise migration from preview identity back to installed identity too.
    for kind, identifier, name in [
        ('debug', 'de.dme86.rst.debug', 'rst Debug'),
        ('release', 'de.dme86.rst.preview', 'rst Preview'),
        ('installed', 'de.dme86.rst', 'rst'),
    ]:
        for _ in range(2):
            subprocess.run(['sh', str(root / 'scripts/macos-bundle-identity.sh'), str(app), kind],
                           env=dict(os.environ, RST_REGISTER_APP='0'), check=True, capture_output=True)
            with (contents / 'Info.plist').open('rb') as file:
                info = plistlib.load(file)
            assert info['CFBundleIdentifier'] == identifier
            assert info['CFBundleName'] == info['CFBundleDisplayName'] == name
            subprocess.run(['codesign', '--verify', '--strict', str(app)], check=True)
        identifiers.add(identifier)
        print(f'{kind}: distinct identity, repeated packaging and bound signature OK')
    assert len(identifiers) == 3

# Old artifacts must migrate out of the ambiguous rst.app paths, idempotently.
with tempfile.TemporaryDirectory(prefix='rst-migration-') as temporary:
    fixture_root = Path(temporary)
    (fixture_root / 'scripts').mkdir()
    shutil.copy2(root / 'scripts/macos-bundle-identity.sh', fixture_root / 'scripts')
    for profile in ('debug', 'release'):
        contents = fixture_root / 'target' / profile / 'rst.app/Contents'
        (contents / 'MacOS').mkdir(parents=True)
        shutil.copyfile(root / 'packaging/macos/Info.plist', contents / 'Info.plist')
        shutil.copy2(root / 'target/release/rst', contents / 'MacOS/rst')
    for _ in range(2):
        subprocess.run(['sh', str(root / 'scripts/migrate-macos-bundles.sh'), str(fixture_root)],
                       env=dict(os.environ, RST_REGISTER_APP='0'), check=True, capture_output=True)
    for profile, name, identifier in [
        ('debug', 'rst Debug', 'de.dme86.rst.debug'),
        ('release', 'rst Preview', 'de.dme86.rst.preview'),
    ]:
        directory = fixture_root / 'target' / profile
        assert not (directory / 'rst.app').exists()
        app = directory / f'{name}.app'
        with (app / 'Contents/Info.plist').open('rb') as file:
            assert plistlib.load(file)['CFBundleIdentifier'] == identifier
        subprocess.run(['codesign', '--verify', '--strict', str(app)], check=True)
    print('Legacy build names: migrated to distinct paths; repeat migration is safe')
