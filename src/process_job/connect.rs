//! Nonblocking FROM OUTSET, including full AF_UNIX listener backlogs. The original
//! source-job deadline covers creation, readiness and discovery probes alike.
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

fn until<T>(
    end: Instant,
    mut now: impl FnMut() -> Instant,
    mut attempt: impl FnMut() -> io::Result<Option<T>>,
    mut wait: impl FnMut(),
) -> io::Result<T> {
    loop {
        if now() >= end {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source connection deadline elapsed",
            ));
        }
        let result = attempt()?;
        if now() >= end {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source connection deadline elapsed",
            ));
        }
        if let Some(stream) = result {
            return Ok(stream);
        }
        wait();
    }
}
pub(crate) fn connect(path: &Path) -> io::Result<UnixStream> {
    let end = super::deadline();
    let mut connection = None;
    until(
        end,
        Instant::now,
        || {
            if connection.is_none() {
                connection = Some(Connection::new(path)?);
            }
            connection.as_mut().unwrap().step()
        },
        || std::thread::park_timeout(Duration::from_millis(1)),
    )
}
struct Connection {
    stream: Option<UnixStream>,
    address: libc::sockaddr_un,
    length: libc::socklen_t,
    pending: bool,
}
impl Connection {
    fn new(path: &Path) -> io::Result<Self> {
        // SAFETY: sockaddr_un contains only integer fields; all-zero is valid.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = path.as_os_str().as_bytes();
        if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid source socket path",
            ));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        let length = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
            as libc::socklen_t;
        #[cfg(target_vendor = "apple")]
        {
            address.sun_len = length as u8;
        }
        // SAFETY: socket returns a fresh descriptor; OwnedFd takes sole ownership.
        let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: this owned descriptor is valid; F_SETFD takes an integer flag.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = UnixStream::from(fd);
        stream.set_nonblocking(true)?; // MUST precede the very first connect call.
        Ok(Self {
            stream: Some(stream),
            address,
            length,
            pending: false,
        })
    }
    fn step(&mut self) -> io::Result<Option<UnixStream>> {
        let stream = self.stream.as_ref().unwrap();
        if !self.pending {
            // SAFETY: address points to our initialized sockaddr_un for length bytes;
            // the descriptor is owned/live and already O_NONBLOCK.
            let result = unsafe {
                libc::connect(
                    stream.as_raw_fd(),
                    (&self.address as *const libc::sockaddr_un).cast(),
                    self.length,
                )
            };
            if result == 0 {
                return Ok(self.stream.take());
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EISCONN) => return Ok(self.stream.take()),
                Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR) => self.pending = true,
                // Linux full UNIX backlogs return EAGAIN without starting a connect.
                Some(libc::EAGAIN) => return Ok(None),
                _ => return Err(error),
            }
        }
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one initialized pollfd, zero timeout: never waits for readiness.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if ready == 0 {
            return Ok(None);
        }
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
        match stream.peer_addr() {
            Ok(_) => Ok(self.stream.take()),
            Err(error) if error.raw_os_error() == Some(libc::ENOTCONN) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn backend_review_r3_pending_connect_has_original_deadline_and_expired_has_zero_attempts() {
        let start = Instant::now();
        let time = Cell::new(start);
        let attempts = Cell::new(0);
        let result: io::Result<()> = until(
            start + Duration::from_millis(3),
            || time.get(),
            || {
                attempts.set(attempts.get() + 1);
                Ok(None)
            },
            || time.set(time.get() + Duration::from_millis(1)),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(attempts.get(), 3);
        let result: io::Result<()> = until(
            time.get(),
            || time.get(),
            || panic!("expired must not open/connect/probe"),
            || panic!("no wait"),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            until(
                time.get() + Duration::from_millis(1),
                || time.get(),
                || Ok(Some(7)),
                || panic!("ready")
            )
            .unwrap(),
            7
        );
    }

    #[test]
    fn backend_review_r3_private_unaccepted_socket_is_nonblocking_from_first_connect() {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("bc-full-{}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        // SAFETY: private owned listener; reduce its backlog for this fixture only.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let mut held = Vec::new();
        let mut pending = None;
        let mut refused = false;
        for _ in 0..64 {
            let mut connection = Connection::new(&path).unwrap();
            // SAFETY: the fixture owns this live descriptor. Verify before FIRST connect.
            assert_ne!(
                unsafe {
                    libc::fcntl(
                        connection.stream.as_ref().unwrap().as_raw_fd(),
                        libc::F_GETFL,
                    )
                } & libc::O_NONBLOCK,
                0
            );
            match connection.step() {
                Ok(Some(stream)) => held.push(stream),
                Ok(None) => {
                    pending = Some(connection);
                    break;
                }
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
                    refused = true;
                    break;
                }
            }
        }
        assert!(!held.is_empty());
        eprintln!(
            "private nonaccepting listener: {} immediate connects, pending={}, refused={}; deterministic seam covers deadline even where native backlog does not saturate at64",
            held.len(),
            pending.is_some(),
            refused
        );
        if let Some(mut pending) = pending {
            let result = until(
                Instant::now() + Duration::from_millis(5),
                Instant::now,
                || pending.step(),
                std::thread::yield_now,
            );
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        }
        drop(held);
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn backend_review_r3_pending_connection_timeout_retains_then_recovers_same_worker_lease() {
        use crate::slow_job::{JobOutcome, SourceJobs};
        use std::sync::mpsc;
        let path = std::env::temp_dir().join(format!("bc-next-{}", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let target = path.clone();
        let (release, gate) = mpsc::sync_channel(1);
        let (started, seen) = mpsc::sync_channel(1);
        let mut jobs = SourceJobs::spawn(move || {
            move |healthy: bool, end| {
                if healthy {
                    return super::super::with_deadline(end, || connect(&target)).map(|_| 7);
                }
                started.send(()).unwrap();
                let _ = gate.recv();
                let fake = Cell::new(end - Duration::from_millis(3));
                until(
                    end,
                    || fake.get(),
                    || Ok(None::<usize>),
                    || fake.set(fake.get() + Duration::from_millis(1)),
                )
            }
        })
        .unwrap();
        let start = Instant::now();
        let end = start + Duration::from_millis(20);
        let first = jobs.submit(false, end).unwrap();
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(jobs.poll(end),Some(JobOutcome::TimedOut{id}) if id==first));
        assert!(
            jobs.submit(true, Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        while jobs.is_busy() {
            assert!(
                jobs.poll(Instant::now()).is_none(),
                "late timeout result cannot apply"
            );
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        let second = jobs
            .submit(true, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(second > first);
        loop {
            if let Some(outcome) = jobs.poll(Instant::now()) {
                assert!(matches!(outcome,JobOutcome::Completed{id,result:Ok(7)} if id==second));
                break;
            }
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        assert!(listener.accept().is_ok());
        drop(jobs);
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
}
