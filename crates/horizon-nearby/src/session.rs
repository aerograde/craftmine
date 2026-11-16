//! One WebRTC session, from the offer to the game's memory pipe.
//!
//! The session thread owns the peer and does the blocking handshake. The
//! service only routes sealed signalling to it and hands the host the
//! server-side pipe once the game provides it.
use crate::{
    proto::{self, Answer, Envelope, Offer, SessionKey, Signal},
    pump::{self, Secure},
    transport::{Link, Peer, PeerEvent},
};
use horizon_friends::crypto::{self, Role};
use horizon_os::time::Duration;
use horizon_protocol::connection::Pipe;
use std::{
    io,
    sync::mpsc::{Receiver, Sender, TryRecvError},
    time::Instant,
};
// std's `thread::Builder` always fails on `wasm32-unknown-unknown`
// (`ErrorKind::Unsupported`, "operation not supported on this platform"), which
// is the "could not start the session" the nearby screen showed in a browser:
// the session thread never began. `horizon_os` starts a Web Worker in the
// shared-memory build and a green thread otherwise.
use horizon_os::thread;

/// What the service tells a session thread.
pub enum Command {
    /// The far side's SDP answer, already opened.
    Answer(String),
    /// The host's memory pipe into the integrated server.
    Pipe(Pipe),
}

/// What a session thread tells the service.
pub enum Event {
    /// A sealed message to route to the far side.
    Signal(Envelope),
    /// The guest's half of the pipe; the game connects through it.
    Ready { session: u64, pipe: Pipe },
    /// The secure tunnel is up and carrying the host's pipe.
    Connected { session: u64 },
    Closed {
        session: u64,
        reason: Option<String>,
    },
}

const POLL: Duration = Duration::from_millis(50);

/// The guest: offer, wait for the answer, connect, secure, hand the game a pipe.
#[allow(clippy::too_many_arguments)]
pub fn guest(
    mut peer: Peer,
    key: SessionKey,
    token: String,
    session: u64,
    from: String,
    to: String,
    timeout: Duration,
    commands: Receiver<Command>,
    events: Sender<Event>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("horizon-nearby-guest".into())
        .spawn(move || {
            let deadline = Instant::now() + timeout;
            let mut offered = false;
            let mut connected = false;
            while !connected {
                for event in peer.poll() {
                    match event {
                        PeerEvent::LocalOffer(sdp) if !offered => {
                            let signal = Signal::Offer(Offer {
                                v: proto::VERSION,
                                msg_id: proto::random_id().unwrap_or_default(),
                                ts: proto::now_ms(),
                                session,
                                sdp,
                            });
                            if let Ok(envelope) = key.seal(&from, &to, session, &signal) {
                                let _ = events.send(Event::Signal(envelope));
                            }
                            offered = true;
                        }
                        PeerEvent::Connected => connected = true,
                        PeerEvent::Closed(reason) => {
                            let _ = events.send(Event::Closed { session, reason });
                            return;
                        }
                        _ => {}
                    }
                }
                loop {
                    match commands.try_recv() {
                        Ok(Command::Answer(sdp)) => peer.accept_answer(sdp),
                        Ok(Command::Pipe(_)) => {}
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                if Instant::now() > deadline {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some("the host did not answer in time".to_owned()),
                    });
                    return;
                }
                thread::sleep(POLL);
            }
            let (reader, writer) = match secure(peer.channel(), &token, session, Role::Guest) {
                Ok(secure) => secure,
                Err(e) => {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some(e.to_string()),
                    });
                    return;
                }
            };
            let (client, server) = Pipe::pair();
            let pump = match pump::start(server, reader, writer) {
                Ok(pump) => pump,
                Err(e) => {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some(e.to_string()),
                    });
                    return;
                }
            };
            let _ = events.send(Event::Ready {
                session,
                pipe: client,
            });
            let _ = pump.join();
            let _ = events.send(Event::Closed {
                session,
                reason: None,
            });
        })
}

/// The host: answer, connect, wait for the server pipe, secure, carry it.
#[allow(clippy::too_many_arguments)]
pub fn host(
    mut peer: Peer,
    key: SessionKey,
    token: String,
    session: u64,
    from: String,
    to: String,
    timeout: Duration,
    commands: Receiver<Command>,
    events: Sender<Event>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("horizon-nearby-host".into())
        .spawn(move || {
            let deadline = Instant::now() + timeout;
            let mut answered = false;
            let mut connected = false;
            let mut pipe = None;
            while !(connected && pipe.is_some()) {
                for event in peer.poll() {
                    match event {
                        PeerEvent::LocalAnswer(sdp) if !answered => {
                            let signal = Signal::Answer(Answer {
                                v: proto::VERSION,
                                msg_id: proto::random_id().unwrap_or_default(),
                                ts: proto::now_ms(),
                                session,
                                sdp,
                            });
                            if let Ok(envelope) = key.seal(&from, &to, session, &signal) {
                                let _ = events.send(Event::Signal(envelope));
                            }
                            answered = true;
                        }
                        PeerEvent::Connected => connected = true,
                        PeerEvent::Closed(reason) => {
                            let _ = events.send(Event::Closed { session, reason });
                            return;
                        }
                        _ => {}
                    }
                }
                loop {
                    match commands.try_recv() {
                        Ok(Command::Pipe(got)) => pipe = Some(got),
                        Ok(Command::Answer(_)) => {}
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                if Instant::now() > deadline {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some("the guest did not connect in time".to_owned()),
                    });
                    return;
                }
                thread::sleep(POLL);
            }
            let (reader, writer) = match secure(peer.channel(), &token, session, Role::Host) {
                Ok(secure) => secure,
                Err(e) => {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some(e.to_string()),
                    });
                    return;
                }
            };
            let pipe = pipe.expect("the loop waits for it");
            let pump = match pump::start(pipe, reader, writer) {
                Ok(pump) => pump,
                Err(e) => {
                    let _ = events.send(Event::Closed {
                        session,
                        reason: Some(e.to_string()),
                    });
                    return;
                }
            };
            let _ = events.send(Event::Connected { session });
            let _ = pump.join();
            let _ = events.send(Event::Closed {
                session,
                reason: None,
            });
        })
}

fn secure(
    link: io::Result<Link>,
    token: &str,
    session: u64,
    role: Role,
) -> io::Result<(crypto::SecureReader<Link>, crypto::SecureWriter<Link>)> {
    let mut link = link?;
    link.set_secure_timeout(Some(Duration::from_secs(10)));
    let key = crypto::exchange(&mut link, token, "nearby", session)?;
    let reader = link.try_clone()?;
    crypto::secure(reader, link, &key, role)
}
