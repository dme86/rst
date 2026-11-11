# Robustness and runtime performance checks

## Reproducible robustness checks

`cargo test` includes deterministic stateful randomized tests with 32 seeds and
500 operations per seed. The corpus mixes malformed UTF-8, arbitrary bytes,
large CSI arguments, cancelled/incomplete escapes, Unicode, colors, terminal
queries, OSC shell markers and primary/alternate-screen changes. Resizes and
scrollback navigation are interleaved with parsing. Every operation checks
cursor/margin bounds, row dimensions, history limits, viewport bounds, saved
alternate-screen geometry and bounded combining-character storage.

A separate differential check parses the same corpus in one read and in
fragmented reads, comparing cells, history, cursor, replies and command capture.
A 32-session test interleaves approximately 25 MiB of output with independent
titles, captures, history eviction, alternate screens and repeated resizing.
These are terminal-model sessions; this test does not create 32 native windows
or claim to validate GUI tab interaction.

```sh
make stress
# Equivalent, or increase these values for a longer run:
RST_STRESS_SEEDS=256 RST_STRESS_STEPS=2000 cargo test --release robustness_tests
```

The extended run performs 512,000 randomized operations plus the other
robustness tests. Seeds are deterministic and reported on assertion failure.
These checks supplement the oversized OSC, clipboard-boundary and blocked-PTY
writer tests. They are not coverage-guided fuzzing or a substitute for testing
native platform behavior.

The initial run found a stale viewport after CSI 3 J cleared history while
scrolled back. That left `display_row` able to underflow/index outside its rows.
Clearing history now resets the viewport; a minimal regression test covers it.

## Parser-only throughput

```sh
make benchmark-parser
```

This opt-in release test parses about 27.5 MiB of colored log output at 120x40
with 64 history rows. It excludes PTY transport, rendering and shell startup.
It prints the workload, elapsed time and MiB/s; it has no machine-dependent
pass/fail speed threshold.

## Real-window benchmark

Requires Python 3 and a working native macOS or Wayland desktop session:

```sh
make benchmark
# Compare with cursor animation disabled:
python3 scripts/benchmark.py --no-cursor-blink --output target/benchmarks/no-blink.json
# Include an interactive shell's rc initialization before the fixture starts:
python3 scripts/benchmark.py --startup-shell /bin/zsh --output target/benchmarks/zsh.json
```

The script opens three sequential test windows, each of which closes itself.
Leave them untouched. It uses a temporary configuration, 120x40 cells, default
font selection, 10,000 history rows, disabled bells and enabled cursor blinking
(unless explicitly disabled). It does not edit dotfiles. `--startup-shell`
executes the selected shell with `-i -c`, so its existing rc files run normally;
it includes that initialization but not an actual prompt theme render.

Each window runs a controlled PTY child rather than an interactive user prompt:

1. Emit a prompt with OSC 133 B to mark readiness.
2. Allow one second to settle, then sample rst's own CPU time across four idle
   seconds. CPU percentages use one core as 100%; child CPU is excluded.
3. Send 20 cursor-position queries for baseline PTY roundtrip latency.
4. Stream colored logs for three seconds, querying after every approximately
   220 KiB. Each reply fences preceding output through the real parser and
   background input writer. The final reply makes the throughput measurement
   include parser consumption rather than only kernel-buffer acceptance.
5. Record RSS snapshots and exit normally, saving the aggregate application
   profile and all latency samples in the JSON report.

Reports include a binary SHA-256, platform, test settings, individual runs,
startup, CPU, RSS snapshots, throughput and raw latency samples. RSS snapshots
are not peak memory and include fonts/framebuffers and populated scrollback.
The script prints median startup/CPU/throughput and p50/p95 roundtrip latency.
Reports live under ignored `target/benchmarks/`; preserve a report under a new
`--output` name when comparing versions. Compare like settings and hardware,
prefer several runs, and avoid treating one timing fluctuation as a regression.

### What the timing means

Spawn-to-prompt-frame includes process launch, configuration, window creation,
font initialization, PTY startup and submission of a frame containing the
readiness marker. The cross-process launch calculation uses wall-clock times;
within-process durations use monotonic clocks. Do not change the system clock
during a run. A submitted frame is not proof of physical display presentation.
The controlled fixture excludes normal shell rc and theme work unless specified.

PTY roundtrip latency measures output parsing, event delivery and reply writing
under the stated workload. It does not measure a physical keypress-to-photon
pipeline. Query fences periodically drain backlog, so this workload is not an
unrestricted flood or a worst-case latency guarantee. Cursor/SGR animation,
window size, scale factor, installed fonts and desktop load affect results.

## Profile an ordinary shell session

```sh
mkdir -p target/benchmarks
RST_PROFILE=target/benchmarks/session-unique.json ./target/release/rst
```

The profile is written on normal exit and refuses to overwrite an existing
file. No text, keys, URLs or clipboard contents are recorded. It contains first
output/frame timing, first parsed/submitted prompt timing, output bytes/chunks,
maximum output-event queue delay, frame counts/duration and event-loop wait
counts. Prompt timing requires shell integration emitting OSC 133 B; without it
the corresponding fields are `null`. In ordinary runs without `RST_PROFILE`,
no profile file or per-output/frame timing samples are created.

The wait count alone is not a CPU measurement. Frame maxima and queue maxima
cover the whole session, including startup. The benchmark separately measures
CPU with OS process counters and reports p50/p95 roundtrips from the PTY child.

## Measurements on the development Mac (2026-11-11)

Release build, three runs per setting, system fonts and default font size,
120x40 cells, 10,000 history rows. These runs were sequential and filesystem
caches were not flushed. Times depend on the local desktop and installed fonts.

| Setting | Median spawn → prompt-frame submission | Median idle CPU (one core) | Load roundtrip p95 | Median sustained parsed load |
| --- | ---: | ---: | ---: | ---: |
| Controlled fixture, cursor blinking | 319 ms | 4.98% | 3.49 ms | 10.3 MiB/s |
| Controlled fixture, no cursor blinking | 277 ms | 0.00%* | 3.28 ms | 10.5 MiB/s |
| zsh interactive rc startup + fixture, cursor blinking | 331 ms | 5.22% | 3.48 ms | 10.4 MiB/s |

*No measurable CPU increment at the process counter's resolution during the
four-second interval; this does not imply mathematically zero CPU use.
The baseline idle PTY roundtrip p95 was about 22 ms. Under load, more frequent
event delivery reduces this latency in this workload; it is not a claim that
all interaction improves under arbitrary load.

The parser-only workload measured 67.3 MiB/s, excluding rendering and transport.
The extended robustness run passed 512,000 randomized operations plus the
multi-session and read-boundary checks.

The cursor comparison identifies a remaining performance opportunity: cursor
blinks currently repaint the software-rendered window. Updating only damaged
regions or caching unchanged terminal pixels could reduce that idle cost.
This change adds measurements and the discovered crash fix; it does not silently
disable cursor blinking or claim that idle repaint optimization is complete.
