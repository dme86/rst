#!/usr/bin/env python3
"""Real-window runtime benchmark; standard library only, macOS and Linux/Wayland."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import select
import shlex
import statistics
import subprocess
import sys
import tempfile
import time


def write_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def write_all(data):
    view = memoryview(data)
    while view:
        view = view[os.write(1, view):]


def roundtrip():
    start = time.perf_counter()
    write_all(b'\x1b[6n')
    response = b''
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
            data = os.read(0, 4096)
            if not data:
                raise RuntimeError('PTY input closed during roundtrip')
            response = (response + data)[-4096:]
            if re.search(rb'\x1b\[\d+;\d+R', response):
                return (time.perf_counter() - start) * 1000
        else:
            break
    raise RuntimeError('terminal did not answer a cursor-position query within 5 seconds')


def fixture(directory):
    import tty
    tty.setraw(0)
    write_all(b'\x1b]133;A\x07rst-benchmark> \x1b]133;B\x07')
    write_json(directory / 'phase.json', {'phase': 'idle'})
    deadline = time.monotonic() + 60
    while not (directory / 'load').exists():
        if time.monotonic() > deadline:
            raise RuntimeError('benchmark controller timed out')
        time.sleep(.02)
    idle_rtt = [roundtrip() for _ in range(20)]
    line = b'\x1b[32mINFO\x1b[m request completed in 123 ms /api/example\r\n'
    batch = line * 256
    latencies = []
    written = 0
    start = time.perf_counter()
    while time.perf_counter() - start < 3:
        for _ in range(16):
            write_all(batch)
            written += len(batch)
        latencies.append(roundtrip())
    # Last query is a fence: all preceding bytes have passed the parser.
    elapsed = time.perf_counter() - start
    write_json(directory / 'workload.json', {
        'bytes': written, 'seconds': elapsed,
        'mib_per_second': written / 1048576 / elapsed,
        'idle_roundtrip_ms': idle_rtt, 'load_roundtrip_ms': latencies,
    })
    time.sleep(.3)  # Allow the final screen update to be submitted.


def cpu_seconds(pid):
    if sys.platform.startswith('linux'):
        # Ignore spaces/parentheses inside the process name.
        fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')
    value = subprocess.check_output(['/bin/ps' if sys.platform == 'darwin' else 'ps', '-p', str(pid), '-o', 'time='], text=True).strip()
    parts = value.split(':')
    return sum(float(part) * 60 ** index for index, part in enumerate(reversed(parts)))


def rss_kib(pid):
    return int(subprocess.check_output(['/bin/ps' if sys.platform == 'darwin' else 'ps', '-p', str(pid), '-o', 'rss='], text=True).strip())


def wait_for(path, process, timeout=30):
    deadline = time.monotonic() + timeout
    while not path.exists():
        if process.poll() is not None:
            raise RuntimeError(f'rst exited early with status {process.returncode}')
        if time.monotonic() > deadline:
            raise RuntimeError(f'timed out waiting for {path.name}')
        time.sleep(.02)


def percentile(values, fraction):
    values = sorted(values)
    return values[min(len(values) - 1, max(0, math.ceil(len(values) * fraction) - 1))]


def run_once(binary, startup_shell, cursor_blink):
    with tempfile.TemporaryDirectory(prefix='rst-benchmark-') as root:
        directory = Path(root)
        config = directory / 'config' / 'rst'
        config.mkdir(parents=True)
        (config / 'config.toml').write_text(
            f'[window]\ncolumns=120\nrows=40\n[cursor]\nblink={str(cursor_blink).lower()}\n'
            '[bell]\nenabled=false\n[scrollback]\nlines=10000\n')
        command = shlex.join([sys.executable, str(Path(__file__).resolve()), '--fixture', str(directory)])
        if startup_shell:
            command = shlex.join([startup_shell, '-i', '-c', 'exec ' + command])
        wrapper = directory / 'shell'
        wrapper.write_text('#!/bin/sh\nexec ' + command + '\n')
        wrapper.chmod(0o700)
        env = dict(os.environ, SHELL=str(wrapper), XDG_CONFIG_HOME=str(directory / 'config'),
                   RST_PROFILE=str(directory / 'profile.json'))
        with (directory / 'stderr.log').open('w+') as errors:
            launch_unix_ms = time.time() * 1000
            process = subprocess.Popen([str(binary)], env=env, stdout=subprocess.DEVNULL,
                                       stderr=errors, start_new_session=True)
            try:
                wait_for(directory / 'phase.json', process)
                time.sleep(1)  # Settle window creation/font loading before sampling idle CPU.
                before = cpu_seconds(process.pid)
                start = time.monotonic()
                idle_rss = rss_kib(process.pid)
                time.sleep(4)
                idle_cpu = max(0, cpu_seconds(process.pid) - before)
                idle_elapsed = time.monotonic() - start
                (directory / 'load').touch()
                wait_for(directory / 'workload.json', process)
                load_rss = rss_kib(process.pid)
                process.wait(timeout=15)
                if process.returncode:
                    raise RuntimeError(f'rst exited with status {process.returncode}')
                profile = json.loads((directory / 'profile.json').read_text())
                workload = json.loads((directory / 'workload.json').read_text())
                if profile['prompt_frame_ms'] is None:
                    raise RuntimeError('no prompt frame was submitted; is a desktop session available?')
                return {
                    'spawn_to_prompt_frame_ms': profile['main_started_unix_ms'] - launch_unix_ms + profile['prompt_frame_ms'],
                    'idle_cpu_percent_one_core': idle_cpu / idle_elapsed * 100,
                    'idle_interval_seconds': idle_elapsed,
                    'idle_rss_kib': idle_rss, 'load_end_rss_kib': load_rss,
                    'profile': profile, 'workload': workload,
                }
            except Exception as error:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                errors.seek(0)
                raise RuntimeError(f'{error}\n{errors.read()[-4000:]}') from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/rst'))
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--output', type=Path, default=Path('target/benchmarks/runtime.json'))
    parser.add_argument('--startup-shell', help='Include this shell\'s interactive rc startup (-i -c); no dotfiles are modified')
    parser.add_argument('--no-cursor-blink', action='store_true', help='Also allow measuring idle with cursor animation disabled')
    parser.add_argument('--fixture', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.fixture:
        fixture(args.fixture)
        return
    if args.runs < 1:
        parser.error('--runs must be positive')
    binary = args.binary.resolve(strict=True)
    samples = []
    for index in range(args.runs):
        print(f'Run {index + 1}/{args.runs}: opening a test window; please leave it untouched.', flush=True)
        samples.append(run_once(binary, args.startup_shell, not args.no_cursor_blink))
    report = {
        'schema': 1, 'platform': platform.platform(), 'python': platform.python_version(),
        'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'startup_shell': args.startup_shell,
        'geometry': '120x40', 'scrollback_lines': 10000, 'cursor_blink': not args.no_cursor_blink,
        'samples': samples,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    write_json(args.output, report)
    print(f"Median spawn to prompt frame: {statistics.median(s['spawn_to_prompt_frame_ms'] for s in samples):.1f} ms")
    print(f"Median idle CPU (one core): {statistics.median(s['idle_cpu_percent_one_core'] for s in samples):.2f}%")
    for name in ('idle', 'load'):
        values = [v for s in samples for v in s['workload'][name + '_roundtrip_ms']]
        print(f'{name.title()} PTY roundtrip: p50 {percentile(values, .5):.2f} ms / p95 {percentile(values, .95):.2f} ms ({len(values)} samples)')
    print(f"Median parsed load: {statistics.median(s['workload']['mib_per_second'] for s in samples):.1f} MiB/s")
    print(f'Report: {args.output.resolve()}')


if __name__ == '__main__':
    main()
