#!/usr/bin/env python3
"""Compare real-window startup with identical PTY workloads (Python 3)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import statistics
import subprocess
import sys
import tempfile
import time


def fixture(result):
    import tty
    tty.setraw(0)
    os.write(1, b'\x1b]133;A\x07startup benchmark> \x1b]133;B\x07\x1b[6n')
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
    Path(result).write_text(str(time.time_ns()))
    # Let the terminal submit the frame before the child closes the window.
    time.sleep(.3)


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
        started = time.time_ns()
        process = subprocess.run(command, env=env, capture_output=True, timeout=30)
        if process.returncode or not (root / 'ready').exists():
            raise RuntimeError(f'{name} failed ({process.returncode}): {process.stderr.decode(errors="replace")[-2000:]}')
        result = {'app': name, 'parse_ready_ms': (int((root / 'ready').read_text()) - started) / 1e6}
        if name != 'alacritty':
            profile = json.loads((root / 'profile.json').read_text())
            if profile['prompt_frame_ms'] is None:
                raise RuntimeError(f'{name} never submitted a prompt frame')
            result['prompt_frame_ms'] = profile['main_started_unix_ms'] - started / 1e6 + profile['prompt_frame_ms']
            result['profile'] = profile
        return result


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--fixture':
        fixture(sys.argv[2])
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/rst'))
    parser.add_argument('--reference-rst', type=Path, help='Optional saved pre-change rst binary')
    parser.add_argument('--alacritty', default=shutil.which('alacritty'))
    parser.add_argument('--user-config', action='store_true', help='Use existing terminal configs instead of matching fixture configs')
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--warmup', type=int, default=1)
    parser.add_argument('--font-family', default='0xProto Nerd Font')
    parser.add_argument('--font-size', type=float, default=16.5)
    parser.add_argument('--fallback', action='append', default=None)
    parser.add_argument('--output', type=Path, default=Path('target/benchmarks/startup.json'))
    args = parser.parse_args()
    if args.runs < 1 or args.warmup < 0 or not args.alacritty:
        parser.error('positive --runs, nonnegative --warmup and an Alacritty executable are required')
    if args.fallback is None:
        args.fallback = ['0xProto Nerd Font Mono', 'JetBrainsMono Nerd Font',
                         'Symbols Nerd Font Mono', 'Menlo', 'Monaco', 'DejaVu Sans Mono']
    binaries = {'rst': args.binary.resolve(), 'alacritty': Path(args.alacritty).resolve()}
    if args.reference_rst:
        binaries['rst-before'] = args.reference_rst.resolve()
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
    report = {'schema': 1, 'platform': sys.platform, 'user_config': args.user_config, 'font_family': None if args.user_config else args.font_family,
              'font_size': None if args.user_config else args.font_size,
              'fallback': None if args.user_config else args.fallback, 'warmup_runs': args.warmup,
              'binary_sha256': fingerprints, 'median_parse_ready_ms': medians, 'runs': results}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print('Median spawn to PTY reply:', ', '.join(f'{name} {value:.1f} ms' for name, value in medians.items()))
    print(f'Report: {args.output}')


if __name__ == '__main__':
    main()
