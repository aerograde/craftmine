//! Richer presence and chunked signalling on the local network.
//!
//! Vanilla's pinger stays on `224.0.2.60:4445` with its short string; nearby
//! uses `4446` for a JSON presence record and for sealed signalling that is
//! too big for one datagram. Everything is link-local: it works with the
//! internet unplugged.
use crate::proto::{Envelope, Presence};
#[cfg(not(target_arch = "wasm32"))]
use horizon_os::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    collections::HashMap,
    net::{Ipv4Addr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Instant,
};
use std::{io, net::SocketAddr, sync::mpsc::Sender};

#[cfg(not(target_arch = "wasm32"))]
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
#[cfg(not(target_arch = "wasm32"))]
pub const PORT: u16 = 4446;
#[cfg(not(target_arch = "wasm32"))]
const MAGIC: &[u8; 4] = b"HZNB";
#[cfg(not(target_arch = "wasm32"))]
const VERSION: u8 = 1;
#[cfg(not(target_arch = "wasm32"))]
const PRESENCE: u8 = 0;
#[cfg(not(target_arch = "wasm32"))]
const CHUNK: u8 = 1;
#[cfg(not(target_arch = "wasm32"))]
const MAX_FRAME: usize = 1200;
#[cfg(not(target_arch = "wasm32"))]
const MAX_CHUNKS: usize = 24;
#[cfg(not(target_arch = "wasm32"))]
const CHUNK_HEADER: usize = 4 + 1 + 1 + 8 + 1 + 1 + 2;
#[cfg(not(target_arch = "wasm32"))]
const REASSEMBLY: Duration = Duration::from_secs(5);

/// What the socket thread hands the service.
#[derive(Debug)]
pub enum Incoming {
    Presence(Presence, SocketAddr),
    /// A whole sealed signalling envelope, JSON.
    Signal(Vec<u8>, SocketAddr),
}

#[cfg(not(target_arch = "wasm32"))]
enum Outgoing {
    Publish(Vec<u8>),
    Unicast(SocketAddr, Vec<u8>),
}

/// The local-network channel, until dropped.
pub struct Lan {
    #[cfg(not(target_arch = "wasm32"))]
    tx: Sender<Outgoing>,
    #[cfg(not(target_arch = "wasm32"))]
    local: SocketAddr,
    #[cfg(not(target_arch = "wasm32"))]
    stop: Arc<AtomicBool>,
    #[cfg(not(target_arch = "wasm32"))]
    thread: Option<horizon_os::thread::JoinHandle<()>>,
}

impl Lan {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(events: Sender<Incoming>) -> io::Result<Self> {
        let socket = bind()?;
        let local = socket.local_addr()?;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = horizon_os::thread::Builder::new()
            .name("horizon-nearby-lan".into())
            .spawn(move || run(socket, events, rx, flag))?;
        Ok(Self {
            tx,
            local,
            stop,
            thread: Some(thread),
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn start(_events: Sender<Incoming>) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a page cannot listen on the local network",
        ))
    }

    /// Announce a presence record to the group.
    pub fn publish(&self, presence: &Presence) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(json) = serde_json::to_vec(presence) {
            let _ = self.tx.send(Outgoing::Publish(json));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = presence;
    }

    /// Send one sealed envelope to the peer at `to` (the source of its
    /// presence datagram).
    pub fn signal(&self, to: SocketAddr, envelope: &Envelope) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(json) = serde_json::to_vec(envelope) {
            let _ = self.tx.send(Outgoing::Unicast(to, json));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (to, envelope);
    }

    /// The address the socket bound, for a preference/debug line.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Lan {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(target_arch = "wasm32"))]
fn bind() -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    let _ = socket.set_multicast_loop_v4(true);
    let _ = socket.set_multicast_ttl_v4(1);
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    Ok(socket)
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(target_arch = "wasm32"))]
fn run(
    socket: UdpSocket,
    events: Sender<Incoming>,
    commands: Receiver<Outgoing>,
    stop: Arc<AtomicBool>,
) {
    let mut partials: HashMap<[u8; 8], Partial> = HashMap::new();
    let mut buf = [0u8; MAX_FRAME];
    while !stop.load(Ordering::Relaxed) {
        loop {
            match commands.try_recv() {
                Ok(Outgoing::Publish(json)) => {
                    if let Some(frame) = presence_frame(&json) {
                        let _ = socket.send_to(&frame, SocketAddr::from((GROUP, PORT)));
                    }
                }
                Ok(Outgoing::Unicast(to, json)) => send_signal(&socket, to, &json),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(received) => received,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                partials.retain(|_, p| p.started.elapsed() < REASSEMBLY);
                continue;
            }
            Err(_) => continue,
        };
        if n < MAGIC.len() + 2 || &buf[..4] != MAGIC || buf[4] != VERSION {
            continue;
        }
        if let Some(incoming) = decode(&buf[..n], from, &mut partials)
            && events.send(incoming).is_err()
        {
            return;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn presence_frame(json: &[u8]) -> Option<Vec<u8>> {
    if json.len() + 6 > MAX_FRAME {
        return None;
    }
    let mut frame = Vec::with_capacity(json.len() + 6);
    frame.extend_from_slice(MAGIC);
    frame.push(VERSION);
    frame.push(PRESENCE);
    frame.extend_from_slice(json);
    Some(frame)
}

#[cfg(not(target_arch = "wasm32"))]
fn send_signal(socket: &UdpSocket, to: SocketAddr, json: &[u8]) {
    let payload = MAX_FRAME - CHUNK_HEADER;
    let total = json.len().div_ceil(payload);
    if total == 0 || total > MAX_CHUNKS {
        return;
    }
    let mut id = [0u8; 8];
    if getrandom::fill(&mut id).is_err() {
        return;
    }
    for (seq, chunk) in json.chunks(payload).enumerate() {
        let mut frame = Vec::with_capacity(CHUNK_HEADER + chunk.len());
        frame.extend_from_slice(MAGIC);
        frame.push(VERSION);
        frame.push(CHUNK);
        frame.extend_from_slice(&id);
        frame.push(seq as u8);
        frame.push(total as u8);
        frame.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        frame.extend_from_slice(chunk);
        // Link-local UDP may drop; two copies cost little for a few KB.
        let _ = socket.send_to(&frame, to);
        let _ = socket.send_to(&frame, to);
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Partial {
    total: usize,
    parts: Vec<Option<Vec<u8>>>,
    started: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
fn decode(
    frame: &[u8],
    from: SocketAddr,
    partials: &mut HashMap<[u8; 8], Partial>,
) -> Option<Incoming> {
    match frame.get(5).copied()? {
        PRESENCE => {
            let mut presence: Presence = serde_json::from_slice(frame.get(6..)?).ok()?;
            presence
                .sanitize()
                .then_some(Incoming::Presence(presence, from))
        }
        CHUNK => {
            if frame.len() < CHUNK_HEADER {
                return None;
            }
            let id: [u8; 8] = frame.get(6..14)?.try_into().ok()?;
            let seq = usize::from(*frame.get(14)?);
            let total = usize::from(*frame.get(15)?);
            let len = usize::from(u16::from_be_bytes([*frame.get(16)?, *frame.get(17)?]));
            if total == 0 || total > MAX_CHUNKS || seq >= total {
                return None;
            }
            let data = frame.get(18..18 + len)?.to_vec();
            let entry = partials.entry(id).or_insert_with(|| Partial {
                total,
                parts: vec![None; total],
                started: Instant::now(),
            });
            if entry.total != total {
                return None;
            }
            entry.parts[seq] = Some(data);
            if entry.parts.iter().any(Option::is_none) {
                return None;
            }
            let entry = partials.remove(&id)?;
            let mut json = Vec::new();
            for part in entry.parts {
                json.extend(part?);
            }
            Some(Incoming::Signal(json, from))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn presence_frames_carry_json() {
        let presence = Presence {
            v: crate::proto::VERSION,
            kind: crate::proto::PresenceKind::World,
            id: crate::proto::random_id().unwrap(),
            name: "World".into(),
            host_id: None,
            world_id: Some(crate::proto::random_id().unwrap()),
            players: Some(1),
            max: Some(8),
            policy: Some(crate::proto::Policy::Ask),
            status: crate::proto::PlayerStatus::Hosting,
            key: "key".into(),
            proto: 777,
            game: "26.3".into(),
            room: "auto-1".into(),
            ts: 1,
        };
        let frame = presence_frame(&serde_json::to_vec(&presence).unwrap()).unwrap();
        let mut partials = HashMap::new();
        let decoded = decode(&frame, "10.0.0.2:4446".parse().unwrap(), &mut partials).unwrap();
        match decoded {
            Incoming::Presence(p, from) => {
                assert_eq!(p, presence);
                assert_eq!(from.port(), 4446);
            }
            _ => panic!("not a presence"),
        }
    }

    #[test]
    fn chunks_reassemble_in_any_order() {
        let json = vec![7u8; 5000];
        let payload = MAX_FRAME - CHUNK_HEADER;
        let total = json.len().div_ceil(payload);
        let id = [3u8; 8];
        let mut frames = Vec::new();
        for (seq, chunk) in json.chunks(payload).enumerate() {
            let mut frame = Vec::new();
            frame.extend_from_slice(MAGIC);
            frame.push(VERSION);
            frame.push(CHUNK);
            frame.extend_from_slice(&id);
            frame.push(seq as u8);
            frame.push(total as u8);
            frame.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            frame.extend_from_slice(chunk);
            frames.push(frame);
        }
        frames.reverse();
        let mut partials = HashMap::new();
        let mut done = None;
        for frame in frames {
            if let Some(incoming) = decode(&frame, "10.0.0.2:4446".parse().unwrap(), &mut partials)
            {
                done = Some(incoming);
            }
        }
        match done.expect("reassembled") {
            Incoming::Signal(got, _) => assert_eq!(got, json),
            _ => panic!("not a signal"),
        }
    }

    #[test]
    fn a_short_chunk_is_refused() {
        let mut partials = HashMap::new();
        assert!(
            decode(
                b"HZNB\x01\x01",
                "10.0.0.2:1".parse().unwrap(),
                &mut partials
            )
            .is_none()
        );
    }
}
