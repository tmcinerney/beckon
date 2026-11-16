//! Bounded single-line request/response pump. The owner dispatches at most one
//! returned request per turn and retains all mutation ownership. No retries.
use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    time::{Duration, Instant},
};

pub const MAX_PEERS: usize = 8;
pub const MAX_ACCEPTS_PER_TURN: usize = 2;
const STEP_BYTES: usize = 4096;
const TURN_BYTES: usize = 16384;
const REQUEST_BYTES: usize = 65536;
const RESPONSE_BYTES: usize = 1048576;

enum State {
    Reading(Vec<u8>),
    Ready(Vec<u8>),
    Waiting,
    Writing { bytes: Vec<u8>, offset: usize },
}
struct Peer<T> {
    id: u64,
    stream: T,
    expires: Instant,
    state: State,
}
#[derive(Debug)]
pub struct IncomingRequest {
    pub id: u64,
    pub bytes: Vec<u8>,
}

pub struct PeerPump<T> {
    peers: VecDeque<Peer<T>>,
    next_id: u64,
}
impl<T> Default for PeerPump<T> {
    fn default() -> Self {
        Self {
            peers: VecDeque::new(),
            next_id: 0,
        }
    }
}
impl<T: Read + Write> PeerPump<T> {
    /// The transport MUST be nonblocking (or a deterministic nonblocking fake).
    /// A caller also caps accepts with MAX_ACCEPTS_PER_TURN.
    pub fn admit(&mut self, stream: T, now: Instant) -> Result<u64, &'static str> {
        if self.peers.len() >= MAX_PEERS {
            return Err("too many clients");
        }
        let id = self.next_id.checked_add(1).ok_or("client id exhausted")?;
        self.next_id = id;
        self.peers.push_back(Peer {
            id,
            stream,
            expires: now + Duration::from_millis(1000),
            state: State::Reading(Vec::new()),
        });
        Ok(id)
    }
    pub fn len(&self) -> usize {
        self.peers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
    pub fn contains(&self, id: u64, now: Instant) -> bool {
        self.peers
            .iter()
            .any(|peer| peer.id == id && now < peer.expires)
    }

    /// Queues a bounded response; does not write, flush or extend peer lifetime.
    pub fn respond(
        &mut self,
        id: u64,
        mut bytes: Vec<u8>,
        now: Instant,
    ) -> Result<(), &'static str> {
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        if bytes.len() > RESPONSE_BYTES {
            return Err("response too large");
        }
        let peer = self
            .peers
            .iter_mut()
            .find(|peer| peer.id == id)
            .ok_or("client gone")?;
        if now >= peer.expires {
            return Err("client expired");
        }
        if !matches!(peer.state, State::Waiting) {
            return Err("response already queued or request incomplete");
        }
        peer.state = State::Writing { bytes, offset: 0 };
        Ok(())
    }

    pub fn poll(&mut self, now: Instant) -> Option<IncomingRequest> {
        let mut request = None;
        let mut remaining = TURN_BYTES;
        for _ in 0..self.peers.len() {
            let mut peer = self.peers.pop_front().expect("bounded peer count");
            if now >= peer.expires {
                continue;
            }
            let mut keep = true;
            match &mut peer.state {
                State::Reading(bytes) => {
                    let mut buffer = [0u8; STEP_BYTES];
                    let limit = STEP_BYTES.min(remaining);
                    if limit > 0 {
                        match peer.stream.read(&mut buffer[..limit]) {
                            Ok(0) => keep = false,
                            Ok(count) => {
                                remaining -= count;
                                bytes.extend_from_slice(&buffer[..count]);
                                if bytes.len() > REQUEST_BYTES {
                                    keep = false;
                                } else if let Some(end) =
                                    bytes.iter().position(|byte| *byte == b'\n')
                                {
                                    peer.state = State::Ready(bytes[..=end].to_vec());
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::Interrupted
                                ) => {}
                            Err(_) => keep = false,
                        }
                    }
                }
                State::Writing { bytes, offset } => {
                    let count = (bytes.len() - *offset).min(STEP_BYTES).min(remaining);
                    if count > 0 {
                        match peer.stream.write(&bytes[*offset..*offset + count]) {
                            Ok(0) => keep = false,
                            Ok(written) => {
                                *offset += written;
                                remaining -= written;
                                if *offset == bytes.len() {
                                    keep = false;
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::Interrupted
                                ) => {}
                            Err(_) => keep = false,
                        }
                    }
                }
                State::Waiting => {
                    let mut extra = [0u8; 1];
                    if remaining > 0 {
                        match peer.stream.read(&mut extra) {
                            Ok(count) => {
                                remaining -= count;
                                keep = false;
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::Interrupted
                                ) => {}
                            Err(_) => keep = false,
                        }
                    }
                }
                State::Ready(_) => {}
            }
            if keep
                && request.is_none()
                && matches!(peer.state, State::Ready(_))
                && let State::Ready(bytes) = std::mem::replace(&mut peer.state, State::Waiting)
            {
                request = Some(IncomingRequest { id: peer.id, bytes });
            }
            if keep {
                self.peers.push_back(peer);
            }
            if remaining == 0 {
                break;
            }
        }
        request
    }
}

impl PeerPump<std::os::unix::net::UnixStream> {
    pub fn admit_unix(
        &mut self,
        stream: std::os::unix::net::UnixStream,
        now: Instant,
    ) -> std::io::Result<u64> {
        stream.set_nonblocking(true)?;
        self.admit(stream, now).map_err(std::io::Error::other)
    }
}
