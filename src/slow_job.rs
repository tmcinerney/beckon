//! One source/navigation job lease with bounded mailboxes. Source-private
//! handlers return immutable data; only the main-thread owner applies results.
//! This is NOT a HID transport actor and owns no device lifetime.
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    time::Instant,
};

struct Work<J> {
    id: u64,
    deadline: Instant,
    job: J,
}
struct Completion<R> {
    id: u64,
    finished_at: Instant,
    result: Option<R>,
}
struct Active {
    id: u64,
    deadline: Instant,
    timed_out: bool,
}
pub enum JobOutcome<R> {
    Completed { id: u64, result: R },
    TimedOut { id: u64 },
    WorkerStopped { id: u64 },
}
pub struct SourceJobs<J, R> {
    requests: SyncSender<Work<J>>,
    results: Receiver<Completion<R>>,
    active: Option<Active>,
    next_id: u64,
}

impl<J: Send + 'static, R: Send + 'static> SourceJobs<J, R> {
    /// Factory runs ON the worker so a source set with RefCell/private caches
    /// need not become shared or Send. No core/config/binding writes in handler.
    pub fn spawn<F, H>(factory: F) -> std::io::Result<Self>
    where
        F: FnOnce() -> H + Send + 'static,
        H: FnMut(J, Instant) -> R + 'static,
    {
        let (tx, rx) = mpsc::sync_channel::<Work<J>>(1);
        let (done, out) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("beckon-source-jobs".into())
            .spawn(move || {
                let mut handler = factory();
                while let Ok(work) = rx.recv() {
                    let result =
                        (Instant::now() < work.deadline).then(|| handler(work.job, work.deadline));
                    if done
                        .try_send(Completion {
                            id: work.id,
                            finished_at: Instant::now(),
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: tx,
            results: out,
            active: None,
            next_id: 0,
        })
    }
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn submit(&mut self, job: J, deadline: Instant) -> Result<u64, &'static str> {
        if self.active.is_some() {
            return Err("source worker busy");
        }
        let id = self
            .next_id
            .checked_add(1)
            .ok_or("source job id exhausted")?;
        self.requests
            .try_send(Work { id, deadline, job })
            .map_err(|_| "source worker unavailable")?;
        self.next_id = id;
        self.active = Some(Active {
            id,
            deadline,
            timed_out: false,
        });
        Ok(id)
    }
    pub fn poll(&mut self, now: Instant) -> Option<JobOutcome<R>> {
        let active = self.active.as_mut()?;
        match self.results.try_recv() {
            Ok(done) => {
                if done.id != active.id {
                    return None;
                }
                let active = self.active.take().expect("active job");
                if active.timed_out {
                    return None;
                }
                if now >= active.deadline
                    || done.finished_at >= active.deadline
                    || done.result.is_none()
                {
                    return Some(JobOutcome::TimedOut { id: active.id });
                }
                Some(JobOutcome::Completed {
                    id: done.id,
                    result: done.result.expect("unexpired command result"),
                })
            }
            Err(TryRecvError::Disconnected) => {
                let active = self.active.take().expect("active job");
                if active.timed_out {
                    None
                } else {
                    Some(JobOutcome::WorkerStopped { id: active.id })
                }
            }
            Err(TryRecvError::Empty) => {
                if now >= active.deadline && !active.timed_out {
                    active.timed_out = true;
                    Some(JobOutcome::TimedOut { id: active.id })
                } else {
                    None
                }
            }
        }
    }
}
// Dropping the request sender retires the worker after its current bounded job.
// There is deliberately no main-thread join, replay or replacement worker.
