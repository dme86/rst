use std::{
    io::{Read, Write},
    thread,
};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use winit::event_loop::EventLoopProxy;

#[derive(Debug)]
pub enum UserEvent {
    Output(Vec<u8>),
    Closed,
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtySession {
    pub fn start(
        cols: usize,
        rows: usize,
        term: &str,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self> {
        let system = native_pty_system();
        let pair = system.openpty(PtySize {
            rows: rows.min(u16::MAX as usize) as u16,
            cols: cols.min(u16::MAX as usize) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut command = CommandBuilder::new(&shell);
        command.env("TERM", term);
        command.env("COLORTERM", "truecolor");
        // Shell hooks activate only inside rst, never in unrelated terminals.
        command.env("TERM_PROGRAM", "rst");
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to start {shell}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        thread::Builder::new()
            .name("rst-pty-reader".into())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if proxy.send_event(UserEvent::Output(buffer[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = proxy.send_event(UserEvent::Closed);
            })?;

        Ok(Self {
            master: pair.master,
            writer,
            _child: child,
        })
    }

    pub fn write(&mut self, data: &[u8]) {
        if let Err(err) = self.writer.write_all(data).and_then(|_| self.writer.flush()) {
            eprintln!("rst: PTY write: {err}");
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_width: usize, cell_height: usize) {
        let size = PtySize {
            rows: rows.min(u16::MAX as usize) as u16,
            cols: cols.min(u16::MAX as usize) as u16,
            pixel_width: (cols * cell_width).min(u16::MAX as usize) as u16,
            pixel_height: (rows * cell_height).min(u16::MAX as usize) as u16,
        };
        if let Err(err) = self.master.resize(size) {
            eprintln!("rst: PTY resize: {err}");
        }
    }
}
