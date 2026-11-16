//! Deadline-scoped command I/O, used only by the command worker (or explicit CLI).
//! Nonblocking parent socket ends avoid pipe-reader threads and inherited-pipe
//! joins. Timeout/overflow kills and reaps the direct child before returning.
use anyhow::{Result, bail};
mod connect;
pub(crate) use connect::connect;
use std::{
    cell::Cell,
    io::{ErrorKind, Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
thread_local! {static DEADLINE:Cell<Option<Instant>>=const {Cell::new(None)};}
pub fn deadline() -> Instant {
    DEADLINE
        .get()
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(1))
}
pub fn with_deadline<T>(at: Instant, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEADLINE.set(self.0);
        }
    }
    let _restore = Restore(DEADLINE.replace(Some(at)));
    run()
}
pub fn output(command: &mut Command) -> Result<Output> {
    output_with_input(command, &[])
}
pub fn output_with_input(command: &mut Command, input: &[u8]) -> Result<Output> {
    let end = deadline();
    if Instant::now() >= end {
        bail!("source job deadline elapsed");
    }
    if input.len() > 1048576 {
        bail!("source command input limit exceeded");
    }
    let (mut stdout, child_stdout) = UnixStream::pair()?;
    let (mut stderr, child_stderr) = UnixStream::pair()?;
    stdout.set_nonblocking(true)?;
    stderr.set_nonblocking(true)?;
    let (mut stdin, child_stdin) = UnixStream::pair()?;
    stdin.set_nonblocking(true)?;
    command
        .stdout(Stdio::from(OwnedFd::from(child_stdout)))
        .stderr(Stdio::from(OwnedFd::from(child_stderr)))
        .stdin(Stdio::from(OwnedFd::from(child_stdin)));
    let mut child = command.spawn()?;
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_done = false;
    let mut err_done = false;
    let mut status = None;
    let mut written = 0;
    let mut input_closed = false;
    let mut captured = 0;
    let result = (|| {
        loop {
            if Instant::now() >= end {
                bail!("source job deadline elapsed");
            }
            if written < input.len() {
                match stdin.write(&input[written..(written + 4096).min(input.len())]) {
                    Ok(0) => bail!("source command closed input"),
                    Ok(count) => written += count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::WouldBlock | ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if written == input.len() && !input_closed {
                stdin.shutdown(std::net::Shutdown::Write)?;
                input_closed = true;
            }
            for (socket, bytes, done) in [
                (&mut stdout, &mut out, &mut out_done),
                (&mut stderr, &mut err, &mut err_done),
            ] {
                let mut buffer = [0u8; 4096];
                match socket.read(&mut buffer) {
                    Ok(0) => *done = true,
                    Ok(count) => {
                        captured += count;
                        if captured > 1048576 {
                            bail!("source command output limit exceeded");
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::WouldBlock | ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if out.len() + err.len() > 1048576 {
                bail!("source command output limit exceeded");
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if out_done
                && err_done
                && let Some(status) = status
            {
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            std::thread::park_timeout(Duration::from_millis(1));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

pub fn request(path: &std::path::Path, request: &serde_json::Value) -> Result<serde_json::Value> {
    let end = deadline();
    let mut stream = connect(path)?;
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    if bytes.len() > 65536 {
        bail!("source request limit exceeded");
    }
    let mut written = 0;
    let mut received = Vec::new();
    loop {
        if Instant::now() >= end {
            bail!("source job deadline elapsed");
        }
        if written < bytes.len() {
            match stream.write(&bytes[written..]) {
                Ok(0) => bail!("source closed request"),
                Ok(count) => written += count,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut buffer = [0u8; 4096];
        match stream.read(&mut buffer) {
            Ok(0) => bail!("source closed without response"),
            Ok(count) => {
                if received.len() + count > 1048576 {
                    bail!("source response limit exceeded");
                }
                received.extend_from_slice(&buffer[..count]);
                if let Some(end) = received.iter().position(|byte| *byte == b'\n') {
                    return Ok(serde_json::from_slice(&received[..end])?);
                }
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backend_review_r3_expired_request_never_attempts_connection() {
        let path = std::env::temp_dir().join(format!("bc-rq-{}", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = with_deadline(Instant::now() - Duration::from_millis(1), || {
            request(&path, &serde_json::json!({"id":1}))
        });
        let accepted = listener.accept().is_ok();
        drop(listener);
        std::fs::remove_file(path).unwrap();
        assert!(result.is_err());
        assert!(
            !accepted,
            "expired work must not even initiate a socket connection"
        );
    }
}
