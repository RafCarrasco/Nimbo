//! Bounded metadata queue and one replaceable completion. No GUI pointers cross this thread.
//! Call shutdown explicitly and handle timeout/panic; Drop signals exit but does not prove a join.
use super::{Catalog, MetadataProvider, NativeMonitor, NativeProcess, NativeWindow};
use crate::picker::RefreshTicket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub type Completion = (RefreshTicket, Result<Catalog, &'static str>);
struct Job {
    ticket: RefreshTicket,
    own: Vec<u32>,
}
pub struct MetadataWorker {
    stop: Arc<AtomicBool>,
    commands: Option<SyncSender<Job>>,
    result: Arc<Mutex<Option<Completion>>>,
    thread: Option<JoinHandle<()>>,
}
impl MetadataWorker {
    pub fn spawn<P: MetadataProvider + Send + 'static>(provider: P) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        let result = Arc::new(Mutex::new(None));
        let output = Arc::clone(&result);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("nimbo-metadata".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let value = Catalog::collect(
                        &Cancellable {
                            provider: &provider,
                            stop: &cancelled,
                        },
                        &job.own,
                    )
                    .map_err(|_| "metadata-enumeration-failed");
                    match output.lock() {
                        Ok(mut slot) => {
                            if cancelled.load(Ordering::Acquire) {
                                break;
                            }
                            *slot = Some((job.ticket, value));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            stop,
            commands: Some(tx),
            result,
            thread: Some(thread),
        })
    }
    pub fn submit(&self, ticket: RefreshTicket, own: Vec<u32>) -> Result<(), &'static str> {
        self.commands
            .as_ref()
            .ok_or("metadata-worker-stopped")?
            .try_send(Job { ticket, own })
            .map_err(|e| match e {
                TrySendError::Full(_) => "metadata-worker-busy",
                TrySendError::Disconnected(_) => "metadata-worker-stopped",
            })
    }
    pub fn take(&self) -> Result<Option<Completion>, &'static str> {
        if self.stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        self.result
            .lock()
            .map(|mut slot| slot.take())
            .map_err(|_| "metadata-worker-poisoned")
    }
    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), &'static str> {
        self.stop.store(true, Ordering::Release);
        self.commands = None;
        if self.thread.is_none() {
            return Ok(());
        }
        let start = Instant::now();
        while !finished(self.thread.as_ref().ok_or("metadata-worker-unavailable")?)? {
            let left = timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err("metadata-worker-stop-timeout");
            }
            thread::sleep(left.min(Duration::from_millis(1)));
        }
        self.thread
            .take()
            .ok_or("metadata-worker-unavailable")?
            .join()
            .map_err(|_| "metadata-worker-panicked")?;
        match self.result.lock() {
            Ok(mut slot) => {
                *slot = None;
                Ok(())
            }
            Err(e) => {
                *e.into_inner() = None;
                Err("metadata-worker-poisoned")
            }
        }
    }
}

// On Windows require actual OS thread termination, not an early Rust completion message.
#[cfg(windows)]
fn finished(thread: &JoinHandle<()>) -> Result<bool, &'static str> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };
    // SAFETY: borrowed handle of our own live JoinHandle; zero timeout; never closed here.
    match unsafe { WaitForSingleObject(HANDLE(thread.as_raw_handle()), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err("metadata-worker-wait-failed"),
    }
}
#[cfg(not(windows))]
fn finished(thread: &JoinHandle<()>) -> Result<bool, &'static str> {
    Ok(thread.is_finished())
}

struct Cancellable<'a, P> {
    provider: &'a P,
    stop: &'a AtomicBool,
}
impl<P: MetadataProvider> MetadataProvider for Cancellable<'_, P> {
    type Error = ();
    fn monitors(&self) -> Result<Vec<NativeMonitor>, ()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(());
        }
        self.provider.monitors().map_err(|_| ())
    }
    fn windows(&self, own: &[u32]) -> Result<Vec<NativeWindow>, ()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(());
        }
        self.provider.windows(own).map_err(|_| ())
    }
    fn processes(&self) -> Result<Vec<NativeProcess>, ()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(());
        }
        self.provider.processes().map_err(|_| ())
    }
}

impl Drop for MetadataWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.commands = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    #[test]
    fn a_completion_signal_is_not_proof_that_provider_destructors_finished() {
        struct BlockingDrop {
            entered: mpsc::Sender<()>,
            release: Receiver<()>,
        }
        impl Drop for BlockingDrop {
            fn drop(&mut self) {
                let _ = self.entered.send(());
                let _ = self.release.recv();
            }
        }
        impl MetadataProvider for BlockingDrop {
            type Error = ();
            fn monitors(&self) -> Result<Vec<NativeMonitor>, ()> {
                Ok(vec![])
            }
            fn windows(&self, _own: &[u32]) -> Result<Vec<NativeWindow>, ()> {
                Ok(vec![])
            }
            fn processes(&self) -> Result<Vec<NativeProcess>, ()> {
                Ok(vec![])
            }
        }
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let mut worker = MetadataWorker::spawn(BlockingDrop {
            entered: entered_tx,
            release,
        })
        .unwrap();
        worker.commands = None; // controlled precondition: loop exits; provider Drop remains blocked.
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let (returned_tx, returned) = mpsc::channel();
        let releaser = thread::spawn(move || {
            let bounded = returned.recv_timeout(Duration::from_millis(300)).is_ok();
            let _ = release_tx.send(());
            bounded
        });
        let result = worker.shutdown(Duration::ZERO);
        let _ = returned_tx.send(());
        let bounded = releaser.join().unwrap();
        assert!(
            result.is_err(),
            "a still-running provider destructor must produce timeout, not a successful join"
        );
        assert!(
            bounded,
            "shutdown blocked inside join after an early completion signal"
        );
        worker.shutdown(Duration::from_secs(2)).unwrap();
    }
}
