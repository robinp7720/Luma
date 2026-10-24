//! Bounded latest-request workers and cancellable, output-draining subprocesses.
use std::cell::RefCell;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;
#[derive(Default)]
struct Queue {
    job: Option<Job>,
    closed: bool,
}
struct Handle {
    queue: Arc<(Mutex<Queue>, Condvar)>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap();
        queue.closed = true;
        queue.job = None;
        ready.notify_one();
    }
}
#[derive(Clone)]
pub(crate) struct LatestWorker(Arc<Handle>);
impl LatestWorker {
    pub fn new() -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = queue.clone();
        std::thread::spawn(move || {
            loop {
                let job = {
                    let (lock, ready) = &*worker_queue;
                    let mut queue = lock.lock().unwrap();
                    while queue.job.is_none() && !queue.closed {
                        queue = ready.wait(queue).unwrap();
                    }
                    if queue.closed {
                        return;
                    }
                    queue.job.take().unwrap()
                };
                job();
            }
        });
        Self(Arc::new(Handle { queue }))
    }
    pub fn submit(&self, cancel: Arc<AtomicBool>, job: impl FnOnce() + Send + 'static) {
        let (lock, ready) = &*self.0.queue;
        lock.lock().unwrap().job = Some(Box::new(move || {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            CANCEL.with(|slot| slot.replace(Some(cancel)));
            job();
            CANCEL.with(|slot| slot.replace(None));
        }));
        ready.notify_one();
    }
}
thread_local! { static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) }; }
pub(crate) fn cancelled() -> bool {
    CANCEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}
pub(crate) trait SearchCommand {
    fn search_output(&mut self) -> io::Result<Output>;
}
impl SearchCommand for Command {
    fn search_output(&mut self) -> io::Result<Output> {
        output(self, Duration::from_secs(3))
    }
}

pub(crate) fn output(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "search superseded",
        ));
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let result = (|| {
        for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
            // Owned pipes: preserve flags while making reads nonblocking.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let deadline = Instant::now() + timeout;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "search superseded",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "search timed out"));
            }
            drain(&mut stdout, &mut out)?;
            drain(&mut stderr, &mut err)?;
            if let Some(status) = child.try_wait()? {
                drain(&mut stdout, &mut out)?;
                drain(&mut stderr, &mut err)?;
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // Cancellation/deadlines stop descendants too. Successful action helpers
    // may intentionally launch another application, which must stay alive.
    if result.is_err() {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait();
    result
}
fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0; 8192];
    // Bound each pass so continuous output cannot starve deadline checks.
    for _ in 0..32 {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if bytes.len() + n > 8 * 1024 * 1024 {
                    return Err(io::Error::other("search output exceeded 8 MiB"));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drains_output_larger_than_pipe_capacity() {
        let result = output(
            Command::new("sh").args([
                "-c",
                "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2",
            ]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 200000);
        assert_eq!(result.stderr.len(), 200000);
    }
    #[test]
    fn terminates_stalled_process() {
        let start = Instant::now();
        let error = output(
            Command::new("sh").args(["-c", "sleep 10"]),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn superseded_request_stops_a_running_subprocess() {
        let worker = LatestWorker::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        worker.submit(cancel.clone(), move || {
            started_tx.send(()).unwrap();
            let result = output(
                Command::new("sh").args(["-c", "sleep 10"]),
                Duration::from_secs(5),
            );
            done_tx.send(result.unwrap_err().kind()).unwrap();
        });
        started_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn pending_requests_are_coalesced() {
        let worker = LatestWorker::new();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        worker.submit(cancel.clone(), move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();
        for n in 0..100 {
            let tx = tx.clone();
            worker.submit(cancel.clone(), move || {
                tx.send(n).unwrap();
            });
        }
        release_tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 99);
        assert!(rx.try_recv().is_err());
    }
}
