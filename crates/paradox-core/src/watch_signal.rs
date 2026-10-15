//! macOS watch-only SIGINT delivery without libdispatch/Foundation startup.
use signal_hook::consts::SIGINT;
use signal_hook::iterator::{Handle, Signals};
use std::io;
use std::thread::{self, JoinHandle};

pub(crate) struct Guard {
    handle: Handle,
    worker: Option<JoinHandle<()>>,
}

impl Guard {
    /// Called only in the final watch process, after any background double-fork.
    /// The worker blocks in the iterator's self-pipe read; it never polls.
    pub(crate) fn new(mut stop: impl FnMut() + Send + 'static) -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT])?;
        let handle = signals.handle();
        let worker = thread::Builder::new()
            .name("paradox-watch-signals".into())
            .spawn(move || {
                for _ in signals.forever() {
                    stop();
                }
            })?;
        Ok(Self {
            handle,
            worker: Some(worker),
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // close wakes the blocking iterator, including when no SIGINT arrived.
        // Join before releasing the handle so stop/idle/startup errors leave
        // neither a worker thread nor registered signal actions behind.
        self.handle.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Captured(Arc<AtomicBool>);
    impl Drop for Captured {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_guard_wakes_and_joins_worker_without_receiving_a_signal() {
        let finished = Arc::new(AtomicBool::new(false));
        let captured = Captured(finished.clone());
        let guard = Guard::new(move || {
            let _ = &captured;
        })
        .unwrap();
        assert!(!finished.load(Ordering::SeqCst));
        drop(guard);
        assert!(finished.load(Ordering::SeqCst));
    }
}
