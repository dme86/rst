# Terminal robustness and startup review (2026-11-11)

The review covered PTY output delivery, VTE escape parsing, command capture,
font loading, glyph caching, and the application's native unsafe calls.
No direct out-of-bounds native buffer write was identified in the reviewed
application code. This is a targeted source review, not a memory-safety
certification or an audit of every dependency.

## Changes

- PTY output events now hold one of 128 credits per session. With 8 KiB reads,
  at most 1 MiB of output is queued per session; a fast producer waits for
  the consumer rather than growing the GUI event queue indefinitely. Credits
  return when events are dropped, including events for already closed tabs.
- VTE's default growable OSC buffer was replaced with its fixed-capacity
  parser (96 KiB). Unterminated OSC strings can no longer grow indefinitely.
  Full-buffer commands are rejected by the OSC handler. The capacity still
  accommodates the supported base64-encoded 64 KiB clipboard payload.
- Both rasterized glyph caches have a 4096-entry limit. Hits stay cached;
  inserting a new glyph at capacity clears that cache. This bounds entry
  counts, not a fixed byte budget (bitmap size depends on font and scale).
- PTY pixel-size arithmetic saturates before conversion to 16-bit dimensions.
- Style fonts are discovered at startup and parsed once on first use.
  Regular faces are deduplicated by font database ID. Font source clones
  share immutable bytes, and plain ASCII detection uses the byte fast path.
- Existing Clippy findings were resolved; explicit geometry functions retain
  narrowly scoped argument-count exceptions.

## Startup measurement

Run the opt-in measurement with:

```sh
cargo test --release startup_benchmark -- --ignored --nocapture
```

On this macOS host, with the same installed fonts and five regular fallback
faces, five consecutive renderer constructions measured:

| Version | First sample | Median of five samples |
| --- | ---: | ---: |
| Original code | 352 ms | 308 ms |
| Lazy style parsing | 127 ms | 107 ms |

This is about a 65% reduction in **renderer font initialization**, not a
measurement of the whole application or shell startup. Filesystem caches were
not flushed. First use of each bold/italic face now pays its parsing cost;
subsequent uses and font zoom reuse it. Font discovery still scans system fonts.

## Validation and limits

Regression checks cover oversized OSC recovery across multiple PTY reads,
the maximum supported clipboard payload, output-credit release, lazy style
loading, and glyph-cache eviction, in addition to the existing terminal tests.
Run `cargo test`, `cargo test --release`, `cargo clippy --all-targets -- -D warnings`,
and `cargo build --release`.

Tests and benchmarks here ran on macOS. Wayland GUI behavior, native IME,
end-to-end startup timing, dependency advisories, and long-running interactive
load testing were not validated by this review. PTY input writes remain
synchronous; a child that stops reading a large paste can still delay the UI.
