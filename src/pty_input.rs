//! Ordered, bounded PTY input. Only the worker may block on the child.
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    thread,
};

const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 1024;

struct Input {
    bytes: Vec<u8>,
    pending: Arc<AtomicUsize>,
}

impl Drop for Input {
    fn drop(&mut self) {
        self.pending.fetch_sub(self.bytes.len(), Ordering::Relaxed);
    }
}

pub struct PtyInput {
    sender: SyncSender<Input>,
    pending: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
}

impl PtyInput {
    pub fn start(
        mut writer: Box<dyn Write + Send>,
        name: String,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Input>(MAX_PENDING_WRITES);
        let pending = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopped = cancelled.clone();
        thread::Builder::new().name(name).spawn(move || {
            for input in receiver {
                // Check between chunks so closing a session discards pending input.
                for chunk in input.bytes.chunks(8192) {
                    if stopped.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Err(err) = writer.write_all(chunk) {
                        on_error(format!("PTY input failed: {err}"));
                        return;
                    }
                }
                if let Err(err) = writer.flush() {
                    on_error(format!("PTY input failed: {err}"));
                    return;
                }
            }
        })?;
        Ok(Self {
            sender,
            pending,
            cancelled,
        })
    }

    /// Accept all parts as one transaction, or reject all of them. In
    /// particular, a refused paste must never leave bracketed paste open.
    pub fn enqueue(&self, parts: &[&[u8]]) -> io::Result<()> {
        let full = || {
            io::Error::new(io::ErrorKind::WouldBlock,
            "Input queue full (16 MiB / 1024 writes); input was not sent. Try again once the application reads input.")
        };
        let len = parts
            .iter()
            .try_fold(0usize, |len, part| len.checked_add(part.len()))
            .ok_or_else(full)?;
        if len == 0 {
            return Ok(());
        }
        self.pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending
                    .checked_add(len)
                    .filter(|total| *total <= MAX_PENDING_BYTES)
            })
            .map_err(|_| full())?;
        let mut input = Input {
            bytes: Vec::with_capacity(len),
            pending: self.pending.clone(),
        };
        for part in parts {
            input.bytes.extend_from_slice(part);
        }
        match self.sender.try_send(input) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(full()),
            Err(TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PTY input is closed; input was not sent",
            )),
        }
    }
}

impl Drop for PtyInput {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        // Never join: a child may still be blocking the worker's current write.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    struct GatedWriter {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        flushed: mpsc::Sender<Vec<u8>>,
        bytes: Vec<u8>,
        blocked: bool,
    }

    impl Write for GatedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.blocked {
                self.blocked = true;
                self.entered.send(()).unwrap();
                self.release.recv_timeout(TIMEOUT).unwrap();
            }
            // Exercise write_all's handling of short writes too.
            let n = bytes.len().min(997);
            self.bytes.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushed.send(self.bytes.clone()).unwrap();
            Ok(())
        }
    }

    struct Fixture {
        input: PtyInput,
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        flushed: mpsc::Receiver<Vec<u8>>,
    }

    fn fixture() -> Fixture {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (flushed, output) = mpsc::channel();
        let writer = GatedWriter {
            entered,
            release: gate,
            flushed,
            bytes: vec![],
            blocked: false,
        };
        let input =
            PtyInput::start(Box::new(writer), "test-input".into(), |err| panic!("{err}")).unwrap();
        Fixture {
            input,
            entered: started,
            release,
            flushed: output,
        }
    }

    #[test]
    fn blocked_child_does_not_block_enqueue_and_paste_stays_ordered() {
        let f = fixture();
        f.input.enqueue(&[b"first"]).unwrap();
        f.entered.recv_timeout(TIMEOUT).unwrap();
        let (done, enqueued) = mpsc::channel();
        // The producer must finish BEFORE the writer is released.
        let producer = thread::spawn(move || {
            f.input
                .enqueue(&[b"\x1b[200~", &vec![b'x'; 20000], b"\x1b[201~"])
                .unwrap();
            f.input.enqueue(&[b"last"]).unwrap();
            done.send(f.input)
                .unwrap_or_else(|_| panic!("producer receiver gone"));
        });
        let input = enqueued.recv_timeout(TIMEOUT).unwrap();
        f.release.send(()).unwrap();
        let mut expected = b"first\x1b[200~".to_vec();
        expected.extend(vec![b'x'; 20000]);
        expected.extend_from_slice(b"\x1b[201~last");
        for _ in 0..2 {
            f.flushed.recv_timeout(TIMEOUT).unwrap();
        }
        assert_eq!(f.flushed.recv_timeout(TIMEOUT).unwrap(), expected);
        producer.join().unwrap();
        drop(input);
    }

    #[test]
    fn byte_limit_counts_in_flight_input_and_rejects_entire_paste() {
        let f = fixture();
        f.input.enqueue(&[&vec![b'x'; MAX_PENDING_BYTES]]).unwrap();
        f.entered.recv_timeout(TIMEOUT).unwrap();
        let err = f
            .input
            .enqueue(&[b"\x1b[200~", b"paste", b"\x1b[201~"])
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(f.input.pending.load(Ordering::Relaxed), MAX_PENDING_BYTES);
        f.release.send(()).unwrap();
        let actual = f.flushed.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(actual, vec![b'x'; MAX_PENDING_BYTES]);
    }

    #[test]
    fn message_limit_also_bounds_tiny_writes() {
        let f = fixture();
        f.input.enqueue(&[b"a"]).unwrap();
        f.entered.recv_timeout(TIMEOUT).unwrap();
        for _ in 0..MAX_PENDING_WRITES {
            f.input.enqueue(&[b"b"]).unwrap();
        }
        assert_eq!(
            f.input.enqueue(&[b"c"]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            f.input.pending.load(Ordering::Relaxed),
            MAX_PENDING_WRITES + 1
        );
        // Drop must return while the writer is still blocked, cancelling queued writes.
        drop(f.input);
        f.release.send(()).unwrap();
        assert_eq!(f.flushed.recv_timeout(TIMEOUT).unwrap(), b"a");
        assert!(f.flushed.recv_timeout(TIMEOUT).is_err());
    }

    #[test]
    fn failed_writer_reports_error() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed child"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (error, received) = mpsc::channel();
        let input = PtyInput::start(Box::new(Broken), "test-broken".into(), move |err| {
            error.send(err).unwrap();
        })
        .unwrap();
        input.enqueue(&[b"test"]).unwrap();
        assert!(received
            .recv_timeout(TIMEOUT)
            .unwrap()
            .contains("closed child"));
        drop(input);
    }
}
