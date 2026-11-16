use beckon::ipc::PeerPump;
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    closed: bool,
    input: VecDeque<u8>,
    output: Vec<u8>,
    stall_read: bool,
    stall_write: bool,
    writes: Vec<usize>,
}
#[derive(Clone, Default)]
struct Stream(Rc<RefCell<State>>);
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.borrow_mut();
        if state.closed {
            return Ok(0);
        }
        if state.stall_read || state.input.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = out.len().min(state.input.len());
        for byte in out.iter_mut().take(n) {
            *byte = state.input.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.borrow_mut();
        if state.stall_write {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        state.output.extend_from_slice(bytes);
        state.writes.push(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("pump must not block on flush")
    }
}
fn ready(bytes: &[u8]) -> Stream {
    let stream = Stream::default();
    stream.0.borrow_mut().input.extend(bytes);
    stream
}

#[test]
fn daemon_turn_disconnected_pending_peer_cannot_authorize_a_late_mutation_result() {
    let now = Instant::now();
    let mut pump = PeerPump::default();
    let peer = ready(b"{}\n");
    let id = pump.admit(peer.clone(), now).unwrap();
    pump.poll(now).unwrap();
    assert!(pump.contains(id, now));
    peer.0.borrow_mut().closed = true;
    pump.poll(now);
    assert!(!pump.contains(id, now));
    assert!(pump.respond(id, b"{}\n".to_vec(), now).is_err());
}

#[test]
fn daemon_turn_stalled_request_and_response_do_not_block_other_peers_or_reset_expiry() {
    let now = Instant::now();
    let mut pump = PeerPump::default();
    let stalled_request = Stream::default();
    stalled_request.0.borrow_mut().stall_read = true;
    pump.admit(stalled_request, now).unwrap();
    let stalled_reply = ready(b"{\"op\":\"config_model\"}\n");
    stalled_reply.0.borrow_mut().stall_write = true;
    let id = pump.admit(stalled_reply.clone(), now).unwrap();
    assert_eq!(pump.poll(now).unwrap().id, id);
    pump.respond(id, b"{\"type\":\"ok\",\"data\":{}}\n".to_vec(), now)
        .unwrap();
    let good = ready(b"{\"op\":\"status\"}\n");
    let good_id = pump.admit(good.clone(), now).unwrap();
    assert_eq!(
        pump.poll(now + Duration::from_millis(500)).unwrap().id,
        good_id
    );
    pump.respond(good_id, b"{}\n".to_vec(), now + Duration::from_millis(500))
        .unwrap();
    pump.poll(now + Duration::from_millis(501));
    assert_eq!(good.0.borrow().output, b"{}\n");
    assert!(stalled_reply.0.borrow().output.is_empty());
    pump.poll(now + Duration::from_millis(1000));
    assert_eq!(pump.len(), 0);
    assert!(
        pump.respond(id, b"{}\n".to_vec(), now + Duration::from_millis(1001))
            .is_err()
    );
}
