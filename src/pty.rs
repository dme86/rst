use std::{
    io::Read,
    thread,
    sync::mpsc::{self, SyncSender},
    ops::Deref,
};

use anyhow::{Context, Result};

use crate::process_name;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use winit::event_loop::EventLoopProxy;

#[derive(Debug)]
pub enum UserEvent {
    Output { session_id: u64, bytes: OutputChunk },
    Closed { session_id: u64 },
    InputError { session_id: u64, message: String },
}

// Bound queued output to 1 MiB per session. Returning a credit on drop also
// handles events for closed tabs and a stopped event loop without losing data.
const OUTPUT_CHUNK_BYTES: usize = 8192;
const OUTPUT_QUEUE_CHUNKS: usize = 128;

#[derive(Debug)]
pub struct OutputChunk {
    bytes: Vec<u8>,
    pub queued_at: Option<std::time::Instant>,
    credit: SyncSender<()>,
}

impl Deref for OutputChunk {
    type Target = [u8];
    fn deref(&self) -> &[u8] { &self.bytes }
}

impl Drop for OutputChunk {
    fn drop(&mut self) {
        let _ = self.credit.try_send(());
    }
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: crate::pty_input::PtyInput,
    proxy: EventLoopProxy<UserEvent>,
    session_id: u64,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    shell_name: String,
    foreground_name: Option<String>,
}

/// Seed tab metadata with the same directory passed to the PTY, before the
/// shell has emitted its first OSC 7 update. Never guess from the active tab.
pub fn initial_directory(cwd: Option<&str>) -> Option<String> {
    starting_directory(cwd, std::env::var("HOME").ok().as_deref())
}

fn starting_directory(cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    let valid = |path: &&str| std::path::Path::new(path).is_absolute()
        && std::path::Path::new(path).is_dir()
        && !path.chars().any(char::is_control);
    cwd.filter(valid).or_else(|| home.filter(valid)).map(str::to_owned)
}

fn shell_command(config: &crate::config::TerminalConfig, cwd: Option<&str>, argv: Option<&[String]>) -> CommandBuilder {
    let shell = config.shell_program();
    let (program, arguments): (&str, &[String]) = match argv.and_then(|args| args.split_first()) {
        Some((program, arguments)) => (program, arguments),
        None => (&shell, &config.shell_args),
    };
    let mut command = CommandBuilder::new(program);
    for arg in arguments { command.arg(arg); }
    if let Some(cwd) = initial_directory(cwd) { command.cwd(cwd); }
    command.env("TERM", &config.term);
    if argv.is_none() { command.env("SHELL", &shell); }
    command.env("COLORTERM", "truecolor");
    // Shell hooks activate only inside rst, never in unrelated terminals.
    command.env("TERM_PROGRAM", "rst");
    command
}

impl PtySession {
    pub fn start(
        session_id: u64,
        cols: usize,
        rows: usize,
        config: &crate::config::TerminalConfig,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self> {
        Self::start_in_dir(session_id, cols, rows, config, proxy, None)
    }

    pub fn start_in_dir(
        session_id: u64, cols: usize, rows: usize, config: &crate::config::TerminalConfig,
        proxy: EventLoopProxy<UserEvent>, cwd: Option<&str>,
    ) -> Result<Self> {
        Self::start_in_dir_with_command(session_id, cols, rows, config, proxy, cwd, None)
    }

    /// Start the first terminal session with an explicit argv (e.g. rst -e nmtui).
    /// New tabs and splits continue to launch the user's normal shell.
    pub fn start_with_command(
        session_id: u64, cols: usize, rows: usize, config: &crate::config::TerminalConfig,
        proxy: EventLoopProxy<UserEvent>, argv: &[String],
    ) -> Result<Self> {
        anyhow::ensure!(!argv.is_empty(), "terminal command cannot be empty");
        Self::start_in_dir_with_command(session_id, cols, rows, config, proxy, None, Some(argv))
    }

    fn start_in_dir_with_command(
        session_id: u64, cols: usize, rows: usize, config: &crate::config::TerminalConfig,
        proxy: EventLoopProxy<UserEvent>, cwd: Option<&str>, argv: Option<&[String]>,
    ) -> Result<Self> {
        let system = native_pty_system();
        let pair = system.openpty(PtySize {
            rows: rows.min(u16::MAX as usize) as u16,
            cols: cols.min(u16::MAX as usize) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let command = shell_command(config, cwd, argv);
        let shell_name = std::path::Path::new(&command.get_argv()[0])
            .file_name().and_then(|part| part.to_str()).unwrap_or("sh").to_owned();
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to start {shell_name}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let input_proxy = proxy.clone();
        let writer = crate::pty_input::PtyInput::start(
            pair.master.take_writer()?, format!("rst-pty-writer-{session_id}"),
            move |message| { let _ = input_proxy.send_event(UserEvent::InputError { session_id, message }); },
        )?;
        let session_proxy = proxy.clone();
        let profiling = std::env::var_os("RST_PROFILE").is_some();

        let (credit, available) = mpsc::sync_channel(OUTPUT_QUEUE_CHUNKS);
        for _ in 0..OUTPUT_QUEUE_CHUNKS {
            credit.send(()).expect("credit receiver is alive");
        }
        thread::Builder::new()
            .name(format!("rst-pty-reader-{session_id}"))
            .spawn(move || {
                let mut buffer = [0u8; OUTPUT_CHUNK_BYTES];
                while available.recv().is_ok() {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if proxy.send_event(UserEvent::Output {
                                session_id,
                                bytes: OutputChunk { bytes: buffer[..n].to_vec(), queued_at: profiling.then(std::time::Instant::now), credit: credit.clone() },
                            }).is_err() {
                                break;
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                            let _ = credit.try_send(());
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                let _ = proxy.send_event(UserEvent::Closed { session_id });
            })?;

        Ok(Self {
            master: pair.master,
            writer,
            proxy: session_proxy,
            session_id,
            _child: child,
            shell_name,
            foreground_name: None,
        })
    }

    /// Probe the exact PTY foreground process group, rather than guessing
    /// from the command typed by the user. This is called at a bounded
    /// interval by the GUI event loop only while an application is running.
    /// Returns true only when the displayed executable name has changed.
    pub fn refresh_foreground_name(&mut self) -> bool {
        let next = self.master.process_group_leader()
            .and_then(process_name::executable_name)
            .filter(|name| name != &self.shell_name);
        if next == self.foreground_name {
            return false;
        }
        self.foreground_name = next;
        true
    }

    /// Clear a stale cached executable when the shell returns to its prompt.
    pub fn clear_foreground_name(&mut self) -> bool {
        self.foreground_name.take().is_some()
    }

    pub fn foreground_name(&self) -> Option<&str> {
        self.foreground_name.as_deref()
    }

    pub fn terminate(&mut self) {
        if let Err(err) = self._child.kill() {
            eprintln!("rst: close shell: {err}");
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        self.write_parts(&[data]);
    }

    pub fn write_parts(&mut self, parts: &[&[u8]]) {
        if let Err(err) = self.writer.enqueue(parts) {
            let _ = self.proxy.send_event(UserEvent::InputError {
                session_id: self.session_id, message: err.to_string(),
            });
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_width: usize, cell_height: usize) {
        let size = PtySize {
            rows: rows.min(u16::MAX as usize) as u16,
            cols: cols.min(u16::MAX as usize) as u16,
            pixel_width: cols.saturating_mul(cell_width).min(u16::MAX as usize) as u16,
            pixel_height: rows.saturating_mul(cell_height).min(u16::MAX as usize) as u16,
        };
        if let Err(err) = self.master.resize(size) {
            eprintln!("rst: PTY resize: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_credit_is_held_until_chunk_is_dropped() {
        let (credit, available) = mpsc::sync_channel(1);
        credit.send(()).unwrap();
        available.recv().unwrap();
        let chunk = OutputChunk { bytes: b"output".to_vec(), queued_at: None, credit };
        assert_eq!(&*chunk, b"output");
        assert!(matches!(available.try_recv(), Err(mpsc::TryRecvError::Empty)));
        drop(chunk);
        assert_eq!(available.try_recv(), Ok(()));
    }

    #[test]
    fn dropping_output_after_reader_exits_does_not_block() {
        let (credit, available) = mpsc::sync_channel(1);
        drop(available);
        drop(OutputChunk { bytes: vec![], queued_at: None, credit });
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    #[test]
    fn startup_directory_is_known_before_shell_metadata() {
        let root = std::env::temp_dir();
        let home = root.to_str().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let cwd = cwd.to_str().unwrap();
        assert_eq!(starting_directory(None, Some(home)).as_deref(), Some(home));
        assert_eq!(starting_directory(Some(cwd), Some(home)).as_deref(), Some(cwd));
        assert_eq!(starting_directory(Some("relative"), Some(home)).as_deref(), Some(home));
        assert_eq!(starting_directory(Some("/nonexistent-rst-start-directory"), Some(home)).as_deref(), Some(home));
        assert_eq!(starting_directory(None, None), None);
    }

    #[test]
    fn configured_shell_arguments_and_explicit_commands_are_independent() {
        let config = crate::config::TerminalConfig { term: "xterm-256color".into(),
            shell: "/path with spaces/fish".into(), shell_args: vec!["-l".into(), "literal $(text)".into()] };
        let command = shell_command(&config, Some("/tmp"), None);
        let args: Vec<_> = command.get_argv().iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args, ["/path with spaces/fish", "-l", "literal $(text)"]);
        assert_eq!(command.get_env("SHELL").unwrap(), "/path with spaces/fish");
        assert_eq!(command.get_env("TERM").unwrap(), "xterm-256color");
        let explicit = vec!["example".into(), "--flag".into()];
        let command = shell_command(&config, None, Some(&explicit));
        let args: Vec<_> = command.get_argv().iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args, ["example", "--flag"]);
        let legacy: crate::config::TerminalConfig = toml::from_str("term = 'xterm-256color'").unwrap();
        assert!(legacy.shell.is_empty() && legacy.shell_args.is_empty());
        assert!(!legacy.shell_program().is_empty());
    }
}
