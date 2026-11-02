//! Maps to: CC `utils/bufferedWriter.ts`.
//!
//! A Condvar worker carries Node timers/setImmediate outside a Tokio runtime.
//! The state lock is never held during writeFn; an independent output lock
//! serializes deferred writes and synchronous flushes without blocking write().

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Maps to: CC `utils/bufferedWriter.ts#WriteFn:1`; Result carries a thrown error.
pub type WriteFn = Arc<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

/// Maps to: CC `utils/bufferedWriter.ts#createBufferedWriter` options:9-21.
pub struct BufferedWriterOptions {
    pub write_fn: WriteFn,
    pub flush_interval_ms: u64,
    pub max_buffer_size: usize,
    /// None carries the source Infinity default. Units are JS UTF-16 length.
    pub max_buffer_bytes: Option<usize>,
    pub immediate_mode: bool,
}

impl BufferedWriterOptions {
    /// Rust construction carrier for source destructured defaults:9-21.
    pub fn new(write_fn: WriteFn) -> Self {
        Self {
            write_fn,
            flush_interval_ms: 1000,
            max_buffer_size: 100,
            max_buffer_bytes: None,
            immediate_mode: false,
        }
    }
}

/// Maps to: CC `utils/bufferedWriter.ts#createBufferedWriter` closure:22-28.
#[derive(Default)]
struct State {
    buffer: Vec<String>,
    buffer_bytes: usize,
    flush_timer: Option<Instant>,
    timer_fired: bool,
    pending_overflow: Option<Vec<String>>,
    // Rust worker lifetime carrier; no source retry policy is added.
    shutdown: bool,
}

struct Shared {
    options: BufferedWriterOptions,
    state: Mutex<State>,
    wake: Condvar,
    output: Mutex<()>,
}

struct Owner {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Consumers explicitly call dispose through cleanupRegistry. Dropping
        // the last Rust handle stops the timer carrier, not an extra flush.
        self.shared.state.lock().unwrap().shutdown = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

/// Maps to: CC `utils/bufferedWriter.ts#BufferedWriter:3-7`.
#[derive(Clone)]
pub struct BufferedWriter {
    owner: Arc<Owner>,
}

/// Maps to: CC `utils/bufferedWriter.ts#createBufferedWriter:9-100`.
pub fn create_buffered_writer(options: BufferedWriterOptions) -> io::Result<BufferedWriter> {
    let shared = Arc::new(Shared {
        options,
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        output: Mutex::new(()),
    });
    let worker = if shared.options.immediate_mode {
        None
    } else {
        let shared = shared.clone();
        Some(
            std::thread::Builder::new()
                .name("buffered-writer".into())
                .spawn(move || {
                    loop {
                        let mut state = shared.state.lock().unwrap();
                        loop {
                            if state.shutdown {
                                return;
                            }
                            if state.pending_overflow.is_some() {
                                break;
                            }
                            if let Some(deadline) = state.flush_timer.filter(|_| !state.timer_fired)
                            {
                                if deadline <= Instant::now() {
                                    break;
                                }
                                state = shared
                                    .wake
                                    .wait_timeout(
                                        state,
                                        deadline.saturating_duration_since(Instant::now()),
                                    )
                                    .unwrap()
                                    .0;
                            } else {
                                state = shared.wake.wait(state).unwrap();
                            }
                        }
                        drop(state);
                        let _output = shared.output.lock().unwrap();
                        let mut state = shared.state.lock().unwrap();
                        if state.shutdown {
                            return;
                        }
                        // setImmediate takes pendingOverflow before calling writeFn;
                        // a throwing callback does not restore this detached batch.
                        if let Some(pending) = state.pending_overflow.take() {
                            drop(state);
                            if let Err(_error) = (shared.options.write_fn)(&pending.concat()) {
                                // CC uncaughtException → logForDiagnosticsNoPII /
                                // logEvent, gracefulShutdown.ts:301-311 — joins
                                // with the unported process diagnostics handler.
                                // It neither exits nor poisons later flush().
                            }
                            continue;
                        }
                        if !state.timer_fired
                            && state
                                .flush_timer
                                .is_some_and(|deadline| deadline <= Instant::now())
                        {
                            drop(state);
                            if let Err(_error) = shared.flush() {
                                let mut state = shared.state.lock().unwrap();
                                // The fired Node timeout does not retry on an exception.
                                state.timer_fired = true;
                                // CC uncaughtException diagnostics/analytics at
                                // gracefulShutdown.ts:301-311 — joins with that
                                // unported handler; explicit flush can retry the
                                // retained buffer without replaying this error.
                            }
                        }
                    }
                })?,
        )
    };
    Ok(BufferedWriter {
        owner: Arc::new(Owner {
            shared,
            worker: Mutex::new(worker),
        }),
    })
}

impl State {
    /// Maps to: CC `utils/bufferedWriter.ts#clearTimer:30-35`.
    fn clear_timer(&mut self) {
        self.flush_timer = None;
        self.timer_fired = false;
    }

    /// Maps to: CC `utils/bufferedWriter.ts#scheduleFlush:49-53`.
    fn schedule_flush(&mut self, interval_ms: u64) {
        if self.flush_timer.is_none() {
            self.flush_timer = Some(Instant::now() + Duration::from_millis(interval_ms));
            self.timer_fired = false;
        }
    }

    /// Maps to: CC `utils/bufferedWriter.ts#flushDeferred:60-80`.
    fn flush_deferred(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        if let Some(pending) = self.pending_overflow.as_mut() {
            pending.extend(buffer);
        } else {
            self.pending_overflow = Some(buffer);
        }
        self.buffer_bytes = 0;
        self.clear_timer();
    }
}

impl Shared {
    /// Maps to: CC `utils/bufferedWriter.ts#flush:37-47`.
    /// Caller holds output lock. Detached snapshots let write() stay cheap
    /// during disk I/O; on synchronous failure the source-owned batch survives.
    fn flush(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        let pending = state.pending_overflow.take();
        let buffer = std::mem::take(&mut state.buffer);
        let buffer_bytes = std::mem::take(&mut state.buffer_bytes);
        let timer = state.flush_timer;
        if !buffer.is_empty() {
            state.clear_timer();
        }
        drop(state);
        if let Some(pending) = pending
            && let Err(error) = (self.options.write_fn)(&pending.concat())
        {
            let mut state = self.state.lock().unwrap();
            let mut restored = pending;
            if let Some(newer) = state.pending_overflow.take() {
                restored.extend(buffer);
                restored.extend(newer);
            } else {
                let mut restored_buffer = buffer;
                restored_buffer.append(&mut state.buffer);
                state.buffer = restored_buffer;
                state.buffer_bytes += buffer_bytes;
                if timer.is_some() {
                    state.flush_timer = timer;
                }
            }
            state.pending_overflow = Some(restored);
            return Err(error);
        }
        if !buffer.is_empty()
            && let Err(error) = (self.options.write_fn)(&buffer.concat())
        {
            let mut state = self.state.lock().unwrap();
            // Concurrent overflow belongs after the failing snapshot.
            if let Some(newer) = state.pending_overflow.take() {
                let mut restored = buffer;
                restored.extend(newer);
                state.pending_overflow = Some(restored);
            } else {
                let mut restored = buffer;
                restored.append(&mut state.buffer);
                state.buffer = restored;
                state.buffer_bytes += buffer_bytes;
                if timer.is_some() {
                    state.flush_timer = timer;
                }
            }
            return Err(error);
        }
        Ok(())
    }
}

impl BufferedWriter {
    /// Maps to: CC `utils/bufferedWriter.ts#write:83-94`.
    pub fn write(&self, content: &str) -> io::Result<()> {
        let shared = &self.owner.shared;
        if shared.options.immediate_mode {
            let _output = shared.output.lock().unwrap();
            return (shared.options.write_fn)(content);
        }
        let mut state = shared.state.lock().unwrap();
        state.buffer.push(content.to_owned());
        state.buffer_bytes += content.encode_utf16().count();
        state.schedule_flush(shared.options.flush_interval_ms);
        if state.buffer.len() >= shared.options.max_buffer_size
            || shared
                .options
                .max_buffer_bytes
                .is_some_and(|max| state.buffer_bytes >= max)
        {
            state.flush_deferred();
        }
        drop(state);
        shared.wake.notify_one();
        Ok(())
    }

    /// Maps to: CC `utils/bufferedWriter.ts#flush:37-47,95`.
    pub fn flush(&self) -> io::Result<()> {
        let shared = &self.owner.shared;
        let _output = shared.output.lock().unwrap();
        shared.flush()
    }

    /// Maps to: CC `utils/bufferedWriter.ts#dispose:96-98`; source stays usable.
    pub fn dispose(&self) -> io::Result<()> {
        self.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn recorded_writer() -> (BufferedWriterOptions, Arc<Mutex<Vec<String>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let capture = recorded.clone();
        (
            BufferedWriterOptions::new(Arc::new(move |text| {
                capture.lock().unwrap().push(text.to_owned());
                Ok(())
            })),
            recorded,
        )
    }

    #[test]
    fn buffered_writer_matches_official_defaults_and_dispose_reuse() {
        let (options, recorded) = recorded_writer();
        assert_eq!(options.flush_interval_ms, 1000);
        assert_eq!(options.max_buffer_size, 100);
        assert_eq!(options.max_buffer_bytes, None);
        assert!(!options.immediate_mode);
        let writer = create_buffered_writer(options).unwrap();
        writer.write("a").unwrap();
        writer.write("b").unwrap();
        assert!(recorded.lock().unwrap().is_empty());
        writer.dispose().unwrap();
        writer.write("c").unwrap();
        writer.flush().unwrap();
        assert_eq!(*recorded.lock().unwrap(), vec!["ab", "c"]);
    }

    #[test]
    fn buffered_writer_matches_official_deferred_coalescing_and_utf16_limit() {
        let (mut options, recorded) = recorded_writer();
        options.max_buffer_bytes = Some(4);
        let writer = create_buffered_writer(options).unwrap();
        // Hold only the carrier's output gate to model several JS writes in
        // one tick before setImmediate gets to execute.
        let output = writer.owner.shared.output.lock().unwrap();
        writer.write("😀").unwrap();
        assert!(
            writer
                .owner
                .shared
                .state
                .lock()
                .unwrap()
                .pending_overflow
                .is_none()
        );
        writer.write("😀").unwrap();
        writer.write("next").unwrap();
        assert_eq!(
            writer
                .owner
                .shared
                .state
                .lock()
                .unwrap()
                .pending_overflow
                .as_ref()
                .unwrap()
                .concat(),
            "😀😀next"
        );
        writer.write("z").unwrap();
        assert!(recorded.lock().unwrap().is_empty());
        drop(output);
        writer.dispose().unwrap();
        assert_eq!(*recorded.lock().unwrap(), vec!["😀😀next", "z"]);
    }

    #[test]
    fn buffered_writer_matches_official_timer_without_runtime() {
        let (send, receive) = mpsc::channel();
        let mut options = BufferedWriterOptions::new(Arc::new(move |text| {
            send.send(text.to_owned()).unwrap();
            Ok(())
        }));
        options.flush_interval_ms = 25;
        let writer = create_buffered_writer(options).unwrap();
        writer.write("timed").unwrap();
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            "timed"
        );
        writer.dispose().unwrap();
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn buffered_writer_matches_official_nonblocking_overflow_and_flush_order() {
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let release_receive = Mutex::new(release_receive);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let capture = recorded.clone();
        let mut options = BufferedWriterOptions::new(Arc::new(move |text| {
            if text == "first" {
                entered_send.send(()).unwrap();
                release_receive.lock().unwrap().recv().unwrap();
            }
            capture.lock().unwrap().push(text.to_owned());
            Ok(())
        }));
        options.max_buffer_size = 1;
        let writer = create_buffered_writer(options).unwrap();
        writer.write("first").unwrap();
        entered_receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        // This would deadlock if write held the disk-output mutex.
        writer.write("second").unwrap();
        writer.write("third").unwrap();
        release_send.send(()).unwrap();
        writer.flush().unwrap();
        assert_eq!(*recorded.lock().unwrap(), vec!["first", "secondthird"]);
    }

    #[test]
    fn buffered_writer_matches_official_immediate_and_synchronous_errors() {
        let mut options =
            BufferedWriterOptions::new(Arc::new(|_| Err(io::Error::other("write failed"))));
        options.immediate_mode = true;
        let writer = create_buffered_writer(options).unwrap();
        assert_eq!(writer.write("a").unwrap_err().to_string(), "write failed");
        writer.flush().unwrap();
        let options =
            BufferedWriterOptions::new(Arc::new(|_| Err(io::Error::other("flush failed"))));
        let writer = create_buffered_writer(options).unwrap();
        writer.write("retained").unwrap();
        assert_eq!(writer.flush().unwrap_err().to_string(), "flush failed");
        assert_eq!(
            writer.owner.shared.state.lock().unwrap().buffer.concat(),
            "retained"
        );
    }
    #[test]
    fn buffered_writer_matches_official_timer_failure_then_dispose() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Source oracle: bufferedWriter.ts timer flush throws before clearing
        // buffer; gracefulShutdown.ts:301-311 keeps the process alive. Explicit
        // dispose later retries A successfully: two attempts, writes=["A"].
        let attempts = Arc::new(AtomicUsize::new(0));
        let capture_attempts = attempts.clone();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let capture = recorded.clone();
        let (failed_send, failed_receive) = mpsc::channel();
        let mut options = BufferedWriterOptions::new(Arc::new(move |text| {
            if capture_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                failed_send.send(()).unwrap();
                return Err(io::Error::other("controlled first write failure"));
            }
            capture.lock().unwrap().push(text.to_owned());
            Ok(())
        }));
        options.flush_interval_ms = 10;
        let writer = create_buffered_writer(options).unwrap();
        writer.write("A").unwrap();
        failed_receive.recv_timeout(Duration::from_secs(2)).unwrap();
        // dispose waits on the worker's output lock until failure restoration
        // is complete, so this assertion has no sleep/timing dependency.
        writer.dispose().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*recorded.lock().unwrap(), vec!["A"]);
    }

    #[test]
    fn buffered_writer_matches_official_overflow_failure_then_dispose() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Source oracle: setImmediate clears pendingOverflow before the throw.
        // A is not retried; writing B followed by dispose writes B exactly once.
        let attempts = Arc::new(AtomicUsize::new(0));
        let capture_attempts = attempts.clone();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let capture = recorded.clone();
        let (failed_send, failed_receive) = mpsc::channel();
        let mut options = BufferedWriterOptions::new(Arc::new(move |text| {
            if capture_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                failed_send.send(()).unwrap();
                return Err(io::Error::other("controlled first write failure"));
            }
            capture.lock().unwrap().push(text.to_owned());
            Ok(())
        }));
        options.max_buffer_size = 1;
        let writer = create_buffered_writer(options).unwrap();
        writer.write("A").unwrap();
        failed_receive.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.write("B").unwrap();
        writer.dispose().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*recorded.lock().unwrap(), vec!["B"]);
    }
}
