#!/usr/bin/env python3
"""Compare real-window startup with identical PTY workloads (Python 3)."""
import sys


def fixture(result):
    # This path runs in every terminal child. Do not import the controller's
    # argument parser, hashing, statistics, etc. before emitting the prompt.
    import os
    import tty
    tty.setraw(0)
    payload = b'\x1b]133;A\x07startup benchmark> \x1b]133;B\x07\x1b[6n'
    while payload:
        payload = payload[os.write(1, payload):]
    import re
    import select
    import time
    data = b''
    deadline = time.monotonic() + 10
    while not re.search(rb'\x1b\[\d+;\d+R', data):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([0], [], [], remaining)[0]:
            raise RuntimeError('terminal did not answer the cursor-position query')
        chunk = os.read(0, 4096)
        if not chunk:
            raise RuntimeError('PTY closed before the cursor-position reply')
        data = (data + chunk)[-4096:]
    with open(result, 'w') as ready:
        ready.write(str(time.time_ns()))
    # Let the terminal submit the frame before the child closes the window.
    time.sleep(.3)


if __name__ == '__main__' and len(sys.argv) == 3 and sys.argv[1] == '--fixture':
    fixture(sys.argv[2])
    raise SystemExit


import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time


def run_once(name, binary, args):
    with tempfile.TemporaryDirectory(prefix='rst-startup-') as temporary:
        root = Path(temporary)
        (root / 'rst').mkdir()
        quote = lambda text: json.dumps(text, ensure_ascii=False)
        (root / 'rst/config.toml').write_text(
            '[window]\ncolumns=96\nrows=28\n[font]\n'
            f'family={quote(args.font_family)}\nsize={args.font_size}\n'
            f'fallback={json.dumps(args.fallback, ensure_ascii=False)}\n')
        alacritty_config = root / 'alacritty.toml'
        alacritty_config.write_text(
            '[window.dimensions]\ncolumns=96\nlines=28\n'
            f'[font]\nsize={args.font_size}\n[font.normal]\nfamily={quote(args.font_family)}\n')
        child = [sys.executable, str(Path(__file__).resolve()), '--fixture', str(root / 'ready')]
        if name == 'alacritty':
            command = [str(binary), *([] if args.user_config else ['--config-file', str(alacritty_config)]), '-e', *child]
        else:
            command = [str(binary), '-e', *child]
        env = dict(os.environ, RST_PROFILE=str(root / 'profile.json'))
        if not args.user_config:
            env['XDG_CONFIG_HOME'] = str(root)
        if args.app:
            target = ['-b', 'org.alacritty'] if name == 'alacritty' else [
                '-a', str(args.reference_app if name == 'rst-before' else args.app)]
            if name == 'rst' and args.app_id:
                target = ['-b', args.app_id]
            launcher = ['/usr/bin/open', '-n', '-W', *target,
                        '--env', f'RST_PROFILE={root / "profile.json"}']
            if not args.user_config:
                launcher += ['--env', f'XDG_CONFIG_HOME={root}']
            command = [*launcher, '--args', *command[1:]]
        started = time.time_ns()
        process = subprocess.run(command, env=env, capture_output=True, timeout=60)
        stderr = process.stderr.decode(errors='replace')
        if process.returncode or not (root / 'ready').exists():
            raise RuntimeError(f'{name} failed ({process.returncode}): {stderr[-2000:]}')
        result = {'app': name, 'parse_ready_ms': (int((root / 'ready').read_text()) - started) / 1e6}
        if name != 'alacritty':
            # Older reference binaries counted a frame even if present failed.
            # A parser reply alone does not establish that a frame was submitted.
            if any(error in stderr for error in (
                'rst: present:', 'rst: resize surface:', 'rst: acquire buffer:',
            )):
                raise RuntimeError(f'{name} failed to submit a frame: {stderr[-2000:]}')
            profile = json.loads((root / 'profile.json').read_text())
            if profile['prompt_frame_ms'] is None:
                raise RuntimeError(f'{name} never submitted a prompt frame')
            launch_offset_ms = profile['main_started_unix_ms'] - started / 1e6
            result['first_frame_ms'] = launch_offset_ms + profile['first_frame_ms']
            result['prompt_frame_ms'] = launch_offset_ms + profile['prompt_frame_ms']
            result['profile'] = profile
        if stderr:
            result['stderr'] = stderr
        return result


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog='Without Alacritty, provide --reference-rst (or --reference-app) for an rst-only comparison. '
               'The same child bypasses interactive shell startup and session restoration. '
               'Reported frames are submissions, not physical display presentation.')
    parser.add_argument('--binary', type=Path, default=Path('target/release/rst'))
    parser.add_argument('--app', type=Path, help='macOS: launch this rst.app through Launch Services')
    parser.add_argument('--app-id', help='With --app, launch rst by this bundle identifier instead of path')
    parser.add_argument('--reference-app', type=Path, help='macOS: optional reference rst.app')
    parser.add_argument('--reference-rst', type=Path, help='Optional saved pre-change rst binary')
    comparison = parser.add_mutually_exclusive_group()
    comparison.add_argument('--alacritty', default=shutil.which('alacritty'),
                            help='Alacritty executable (default: discover in PATH; optional with a reference rst)')
    comparison.add_argument('--no-alacritty', action='store_true',
                            help='Compare only rst and its saved reference, even when Alacritty is installed')
    parser.add_argument('--user-config', action='store_true', help='Use existing terminal configs instead of matching fixture configs')
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--warmup', type=int, default=1)
    parser.add_argument('--font-family', default='0xProto Nerd Font')
    parser.add_argument('--font-size', type=float, default=16.5)
    parser.add_argument('--fallback', action='append', default=None)
    parser.add_argument('--output', type=Path, default=Path('target/benchmarks/startup.json'))
    args = parser.parse_args()
    if args.runs < 1 or args.warmup < 0:
        parser.error('positive --runs and nonnegative --warmup are required')
    if args.no_alacritty:
        args.alacritty = None
    if not args.alacritty and not (args.reference_rst or args.reference_app):
        parser.error('provide --reference-rst or --reference-app when Alacritty is unavailable or disabled')
    if args.app:
        args.app = args.app.resolve()
    if args.reference_app:
        args.reference_app = args.reference_app.resolve()
    if args.fallback is None:
        args.fallback = ['0xProto Nerd Font Mono', 'JetBrainsMono Nerd Font',
                         'Symbols Nerd Font Mono', 'Menlo', 'Monaco', 'DejaVu Sans Mono']
    if (args.app or args.reference_app) and sys.platform != 'darwin':
        parser.error('--app and --reference-app require macOS')
    if args.app_id and not args.app:
        parser.error('--app-id requires --app')
    if args.reference_app and not args.app:
        parser.error('--reference-app requires --app')
    if args.app and args.reference_rst:
        parser.error('use --reference-app with --app, not --reference-rst')
    binaries = {'rst': (args.app / 'Contents/MacOS/rst' if args.app else args.binary).resolve()}
    if args.alacritty:
        alacritty = shutil.which(args.alacritty)
        if not alacritty:
            parser.error(f'Alacritty executable not found: {args.alacritty}')
        binaries['alacritty'] = Path(alacritty).resolve()
    if args.reference_app:
        binaries['rst-before'] = (args.reference_app / 'Contents/MacOS/rst').resolve()
    if args.reference_rst:
        binaries['rst-before'] = args.reference_rst.resolve()
    for name, binary in binaries.items():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f'{name} executable not found or not executable: {binary}')
    fingerprints = {name: hashlib.sha256(binary.read_bytes()).hexdigest() for name, binary in binaries.items()}
    results = []
    for index in range(args.warmup + args.runs):
        # Alternate order as well as interleaving binaries to reduce drift bias.
        order = list(binaries.items())
        if index % 2:
            order.reverse()
        for name, binary in order:
            result = run_once(name, binary, args)
            warmup = index < args.warmup
            print(f'{name}: {result["parse_ready_ms"]:.1f} ms' + (' (warmup)' if warmup else ''), flush=True)
            if not warmup:
                result['run'] = index - args.warmup
                results.append(result)
    medians = {name: statistics.median(r['parse_ready_ms'] for r in results if r['app'] == name) for name in binaries}
    frame_medians = {
        metric: {name: statistics.median(r[metric] for r in results if r['app'] == name)
                 for name in binaries if name != 'alacritty'}
        for metric in ('first_frame_ms', 'prompt_frame_ms')
    }
    report = {'schema': 1, 'platform': sys.platform, 'launch_services': bool(args.app), 'launch_bundle_id': args.app_id, 'user_config': args.user_config, 'font_family': None if args.user_config else args.font_family,
              'font_size': None if args.user_config else args.font_size,
              'fallback': None if args.user_config else args.fallback, 'warmup_runs': args.warmup,
              'binary_sha256': fingerprints, 'median_parse_ready_ms': medians,
              'median_first_frame_ms': frame_medians['first_frame_ms'],
              'median_prompt_frame_ms': frame_medians['prompt_frame_ms'], 'runs': results}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print('Median spawn to PTY reply:', ', '.join(f'{name} {value:.1f} ms' for name, value in medians.items()))
    for metric, label in (('first_frame_ms', 'first submitted frame'), ('prompt_frame_ms', 'submitted prompt frame')):
        print(f'Median spawn to {label}:', ', '.join(f'{name} {value:.1f} ms' for name, value in frame_medians[metric].items()))
    print(f'Report: {args.output}')


if __name__ == '__main__':
    main()
