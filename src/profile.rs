//! Opt-in aggregate runtime measurements; never records terminal text or keys.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    time::Instant,
};

pub struct Profile {
    file: File,
    started: Instant,
    main_started_unix_ms: f64,
    first_output_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    prompt_received_ms: Option<f64>,
    prompt_frame_ms: Option<f64>,
    output_bytes: u64,
    output_chunks: u64,
    frames: u64,
    frame_total_ms: f64,
    frame_max_ms: f64,
    queue_max_ms: f64,
    waits: u64,
    cursor_phases: u8,
}

impl Profile {
    pub fn from_env(started: Instant) -> std::io::Result<Option<Self>> {
        let Some(path) = std::env::var_os("RST_PROFILE") else {
            return Ok(None);
        };
        // A mistyped profiling path must not overwrite an existing user file.
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Some(Self {
            file,
            started,
            main_started_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0
                - started.elapsed().as_secs_f64() * 1000.0,
            first_output_ms: None,
            first_frame_ms: None,
            prompt_received_ms: None,
            prompt_frame_ms: None,
            output_bytes: 0,
            output_chunks: 0,
            frames: 0,
            frame_total_ms: 0.0,
            frame_max_ms: 0.0,
            queue_max_ms: 0.0,
            waits: 0,
            cursor_phases: 0,
        }))
    }

    fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub fn output(&mut self, bytes: usize, queued: Option<Instant>) {
        let now = self.elapsed_ms();
        self.first_output_ms.get_or_insert(now);
        self.output_bytes += bytes as u64;
        self.output_chunks += 1;
        if let Some(queued) = queued {
            self.queue_max_ms = self
                .queue_max_ms
                .max(queued.elapsed().as_secs_f64() * 1000.0);
        }
    }

    pub fn prompt(&mut self, seen: bool) {
        if seen && self.prompt_received_ms.is_none() {
            self.prompt_received_ms = Some(self.elapsed_ms());
        }
    }

    pub fn frame(&mut self, draw_started: Instant, prompt_seen: bool) {
        let now = self.elapsed_ms();
        self.first_frame_ms.get_or_insert(now);
        if prompt_seen {
            self.prompt_frame_ms.get_or_insert(now);
        }
        let duration = draw_started.elapsed().as_secs_f64() * 1000.0;
        self.frames += 1;
        self.frame_total_ms += duration;
        self.frame_max_ms = self.frame_max_ms.max(duration);
    }

    pub fn wait(&mut self, cursor_opacity: Option<f32>) {
        self.waits += 1;
        if let Some(opacity) = cursor_opacity {
            if opacity >= 0.99 {
                self.cursor_phases |= 1;
            }
            if opacity <= 0.01 {
                self.cursor_phases |= 2;
            }
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        fn number(value: Option<f64>) -> String {
            value.map_or_else(|| "null".into(), |value| format!("{value:.3}"))
        }
        let result = writeln!(self.file,
            "{{\"schema\":1,\"main_started_unix_ms\":{:.3},\"first_output_ms\":{},\"first_frame_ms\":{},\"prompt_received_ms\":{},\"prompt_frame_ms\":{},\"output_bytes\":{},\"output_chunks\":{},\"frames\":{},\"frame_total_ms\":{:.3},\"frame_max_ms\":{:.3},\"output_queue_max_ms\":{:.3},\"event_loop_waits\":{},\"native_cursor_phases\":{}}}",
            self.main_started_unix_ms, number(self.first_output_ms), number(self.first_frame_ms),
            number(self.prompt_received_ms), number(self.prompt_frame_ms),
            self.output_bytes, self.output_chunks, self.frames, self.frame_total_ms,
            self.frame_max_ms, self.queue_max_ms, self.waits, self.cursor_phases);
        if let Err(err) = result {
            eprintln!("rst: write profile: {err}");
        }
    }
}
