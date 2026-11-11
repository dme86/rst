//! Reproducible stateful fuzz-style tests. Failure messages include the seed.
use super::*;

struct Random(u64);
impl Random {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

fn terminal() -> Terminal {
    let config: crate::config::Config =
        toml::from_str(include_str!("../config/default.toml")).unwrap();
    let mut term = Terminal::new(80, 24, 64, Palette::try_from(&config.colors).unwrap());
    term.screen.command_capture = CommandCapture::new(4096);
    term
}

fn invariants(term: &Terminal, seed: u64) {
    let s = &term.screen;
    assert!(s.cols > 0 && s.rows > 0, "seed {seed}");
    assert!(s.x < s.cols && s.y < s.rows, "cursor: seed {seed}");
    assert_eq!(s.grid.len(), s.rows, "seed {seed}");
    assert!(
        s.scroll_top < s.scroll_bottom && s.scroll_bottom <= s.rows,
        "margins: seed {seed}"
    );
    assert!(s.history.len() <= s.history_limit, "history: seed {seed}");
    assert!(s.view_offset <= s.history.len(), "viewport: seed {seed}");
    assert_eq!(s.tab_stops.len(), s.cols, "tabs: seed {seed}");
    for row in s.grid.iter().chain(s.history.iter()) {
        assert_eq!(row.len(), s.cols, "row width: seed {seed}");
        for cell in row {
            assert!(cell.width <= 2, "cell width: seed {seed}");
            assert!(
                usize::from(cell.combining_len) <= cell.combining.len(),
                "grapheme: seed {seed}"
            );
            let _ = cell.text();
        }
    }
    if let Some(saved) = &s.alternate {
        assert_eq!(saved.grid.len(), s.rows, "alternate rows: seed {seed}");
        assert!(
            saved.x < s.cols && saved.y < s.rows,
            "alternate cursor: seed {seed}"
        );
        assert!(
            saved.grid.iter().all(|row| row.len() == s.cols),
            "alternate width: seed {seed}"
        );
    }
    for y in 0..s.rows {
        assert_eq!(s.display_row(y).len(), s.cols, "display: seed {seed}");
    }
}

fn corpus() -> Vec<&'static [u8]> {
    vec![
        b"plain output\r\n",
        "界e\u{301}👩\u{200d}💻".as_bytes(),
        b"\x1b[99999;99999H",
        b"\x1b[0;9999r",
        b"\x1b[2;3r",
        b"\x1b[?6h",
        b"\x1b[?6l",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[9999L",
        b"\x1b[9999M",
        b"\x1b[9999@",
        b"\x1b[9999P",
        b"\x1b[9999S",
        b"\x1b[9999T",
        b"\x1b[2J",
        b"\x1b[3J",
        b"\x1b[1;3;4;38;2;255;0;42m",
        b"\x1b[m",
        b"\x1b[!p",
        b"\x1bc",
        b"\x1b]133;A\x07",
        b"\x1b]133;B\x07",
        b"\x1b]133;C\x07",
        b"\x1b]133;D\x07",
        b"\x1b]2;title\x1b\\",
        b"\x1b]52;c;!!!\x07",
        b"\x1b]4;1;#fff\x07",
        b"\x1b[6n",
        b"\x1b[?2004$p",
        b"\x1bPignored\x1b\\",
        b"\xff\xfe\xf0\x80\xc0",
        b"\x18\x1a",
        b"\r\n\t\x08",
        b"\x1b(0lqk\x1b(B",
    ]
}

#[test]
fn random_streams_resize_and_scrollback_preserve_invariants() {
    let corpus = corpus();
    let seeds: u64 = std::env::var("RST_STRESS_SEEDS").map_or(32, |value| {
        value.parse().expect("positive RST_STRESS_SEEDS")
    });
    let steps: usize = std::env::var("RST_STRESS_STEPS").map_or(500, |value| {
        value.parse().expect("positive RST_STRESS_STEPS")
    });
    assert!(seeds > 0 && steps > 0);
    for seed in 1..=seeds {
        let mut rng = Random(seed);
        let mut term = terminal();
        for _ in 0..steps {
            match rng.next() % 8 {
                0 => term.screen.resize(rng.next() % 120, rng.next() % 40),
                1 => term.screen.scroll_view((rng.next() % 201) as i32 - 100),
                2 => {
                    let bytes: Vec<u8> = (0..rng.next() % 128).map(|_| rng.next() as u8).collect();
                    term.advance(&bytes);
                }
                _ => {
                    let bytes = corpus[rng.next() % corpus.len()];
                    for chunk in bytes.chunks(1 + rng.next() % 17) {
                        term.advance(chunk);
                    }
                }
            }
            invariants(&term, seed);
            term.screen.take_replies();
            term.screen.take_bells();
            assert!(term.screen.take_clipboard_writes().is_empty());
        }
    }
}

#[test]
fn parser_result_is_independent_of_pty_read_boundaries() {
    let corpus = corpus();
    for seed in 1..=16 {
        let mut rng = Random(seed);
        let mut bytes = Vec::new();
        for _ in 0..200 {
            bytes.extend_from_slice(corpus[rng.next() % corpus.len()]);
        }
        let mut whole = terminal();
        let mut split = terminal();
        whole.advance(&bytes);
        for chunk in bytes.chunks(1 + seed as usize % 31) {
            split.advance(chunk);
        }
        assert_eq!(whole.screen.grid, split.screen.grid, "seed {seed}");
        assert_eq!(whole.screen.history, split.screen.history, "seed {seed}");
        assert_eq!(
            (whole.screen.x, whole.screen.y),
            (split.screen.x, split.screen.y),
            "seed {seed}"
        );
        assert_eq!(whole.screen.title, split.screen.title, "seed {seed}");
        assert_eq!(
            whole.screen.take_replies(),
            split.screen.take_replies(),
            "seed {seed}"
        );
        assert_eq!(
            whole.screen.command_capture.latest_output(),
            split.screen.command_capture.latest_output(),
            "seed {seed}"
        );
    }
}

#[test]
fn thirty_two_sessions_keep_independent_bounded_state_under_output_and_resize() {
    let mut sessions: Vec<_> = (0..32).map(|_| terminal()).collect();
    for round in 0..200 {
        for (id, term) in sessions.iter_mut().enumerate() {
            term.advance(
                format!("\x1b]2;session-{id}\x07\x1b]133;C\x07round {round}\r\n").as_bytes(),
            );
            term.advance(&vec![b'x'; 4096]);
            term.advance(b"\r\n\x1b]133;D\x07");
            if round % 5 == 0 {
                term.advance(b"\x1b[?1049h");
                term.screen
                    .resize(1 + (round + id) % 100, 1 + (round * 3 + id) % 30);
                term.advance(b"TUI\x1b[?1049l");
            }
            assert_eq!(
                term.screen.command_capture.latest_output(),
                Err(crate::command_capture::CopyError::TooLarge)
            );
            assert_eq!(term.screen.title, format!("session-{id}"));
            invariants(term, id as u64);
        }
    }
}

#[test]
#[ignore = "manual release throughput benchmark"]
fn parser_throughput_benchmark() {
    let mut term = terminal();
    term.screen.resize(120, 40);
    let line = b"\x1b[32mINFO\x1b[m request completed in 123 ms /api/example\r\n";
    let batch = line.repeat(128);
    let start = std::time::Instant::now();
    let rounds = 4096;
    for _ in 0..rounds {
        term.advance(std::hint::black_box(&batch));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "parser: {:.1} MiB/s ({} bytes in {:?}; 120x40, 64 history rows)",
        (batch.len() * rounds) as f64 / 1048576.0 / elapsed.as_secs_f64(),
        batch.len() * rounds,
        elapsed
    );
    std::hint::black_box(term);
}

#[test]
fn clearing_scrollback_while_scrolled_back_resets_the_viewport() {
    let mut term = terminal();
    term.screen.resize(10, 3);
    term.advance(b"one\r\ntwo\r\nthree\r\nfour\r\n");
    term.screen.scroll_view(2);
    assert!(term.screen.view_offset > 0);
    term.advance(b"\x1b[3J");
    assert_eq!(term.screen.view_offset, 0);
    assert!(term.screen.history.is_empty());
    invariants(&term, 0);
}
