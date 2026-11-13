//! One `str0m::Rtc` on its own thread, with the data channel as a byte stream.
use crate::{Channel, Config, install_crypto};
use horizon_os::time::Duration;
use log::{debug, warn};
use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::Instant,
};
use str0m::{
    Candidate, Event as RtcEvent, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpOffer, SdpPendingOffer},
    net::{Protocol, Receive},
};

/// What the peer thread reports to the service.
#[derive(Debug)]
pub enum Event {
    /// Our offer, as JSON (`{type:"offer", sdp:"..."}`), after gathering.
    LocalOffer(String),
    /// Our answer, as JSON, after gathering.
    LocalAnswer(String),
    /// The data channel is open and bytes can be written.
    Connected,
    /// The far side's bytes.
    Data(Vec<u8>),
    /// The link ended; `Some` when it was an error rather than a close.
    Closed(Option<String>),
}

/// What the service (or the channel) asks the peer thread.
pub enum Command {
    /// The far side's answer to our offer.
    AcceptAnswer(String),
    /// There is something in the channel's outbound queue.
    Wake,
    Close,
}

/// One WebRTC peer connection, driven on a thread of its own.
pub struct Peer {
    events: Receiver<Event>,
    commands: Sender<Command>,
    channel: Channel,
    _thread: Option<thread::JoinHandle<()>>,
}

impl Peer {
    /// Start as the side that creates the data channel and offers it.
    pub fn offer(config: Config) -> io::Result<Self> {
        Self::start(config, None)
    }

    /// Start as the side that answers `offer`.
    pub fn answer(config: Config, offer: String) -> io::Result<Self> {
        Self::start(config, Some(offer))
    }

    fn start(config: Config, offer: Option<String>) -> io::Result<Self> {
        // ICE discards checks that arrive on an address it has no local
        // candidate for, and a wildcard socket cannot tell it which local
        // address a packet reached. Bind the primary interface instead.
        let bind = bind_address(&config);
        let socket = UdpSocket::bind((bind, 0))?;
        socket.set_read_timeout(Some(Duration::from_millis(10)))?;
        let (event_tx, events) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let channel = Channel::new().with_wake(commands.clone());
        let thread_channel = channel.try_clone();
        let thread = thread::Builder::new()
            .name("horizon-webrtc".into())
            .spawn(move || run(socket, config, offer, command_rx, event_tx, thread_channel))?;
        Ok(Self {
            events,
            commands,
            channel,
            _thread: Some(thread),
        })
    }

    /// A handle on the data channel. Writing wakes the peer thread; reading
    /// blocks (or times out) on the inbound queue.
    pub fn channel(&self) -> Channel {
        self.channel.try_clone()
    }

    /// Everything the peer reported since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Apply the far side's answer to our offer.
    pub fn accept_answer(&self, answer: String) {
        let _ = self.commands.send(Command::AcceptAnswer(answer));
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

/// The primary IPv4 address to bind: the loopback for tests, else the first
/// non-loopback interface (the friends crate ranks private LAN addresses
/// first, then VPN ranges).
fn bind_address(config: &Config) -> Ipv4Addr {
    if config.loopback {
        return Ipv4Addr::LOCALHOST;
    }
    horizon_friends::reach::interface_addresses(false)
        .into_iter()
        .find_map(|ip| match ip {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
            _ => None,
        })
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

fn add_candidates(rtc: &mut Rtc, socket: &UdpSocket, config: &Config) {
    let Ok(local) = socket.local_addr() else {
        return;
    };
    if let Ok(candidate) = Candidate::host(local, "udp") {
        let _ = rtc.add_local_candidate(candidate);
    }
    // A server-reflexive candidate must be the mapping of this socket. Ask
    // each STUN server on it, first answer wins.
    let budget = config.gather.min(Duration::from_secs(2));
    for server in &config.stun {
        let Ok(mapped) = horizon_friends::stun::query_socket(socket, server, budget) else {
            continue;
        };
        if mapped.is_ipv4()
            && let Ok(candidate) = Candidate::server_reflexive(mapped, local, "udp")
        {
            let _ = rtc.add_local_candidate(candidate);
            debug!("horizon-webrtc: server-reflexive candidate {mapped} via {server}");
        }
        break;
    }
    let _ = socket.set_read_timeout(Some(Duration::from_millis(10)));
}

fn emit(tx: &Sender<Event>, event: Event) {
    let _ = tx.send(event);
}

fn run(
    socket: UdpSocket,
    config: Config,
    offer: Option<String>,
    commands: Receiver<Command>,
    events: Sender<Event>,
    channel: Channel,
) {
    install_crypto();
    let mut rtc = Rtc::builder().build(Instant::now());
    add_candidates(&mut rtc, &socket, &config);
    let mut pending: Option<SdpPendingOffer> = None;
    let mut cid = None;
    match offer {
        Some(json) => {
            let json = crate::sdp::rewrite_mdns(&json).unwrap_or(json);
            let offer: SdpOffer = match serde_json::from_str(&json) {
                Ok(offer) => offer,
                Err(e) => {
                    finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                    return;
                }
            };
            match rtc.sdp_api().accept_offer(offer) {
                Ok(answer) => match serde_json::to_string(&answer) {
                    Ok(json) => emit(&events, Event::LocalAnswer(json)),
                    Err(e) => {
                        finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                        return;
                    }
                },
                Err(e) => {
                    finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                    return;
                }
            }
        }
        None => {
            let mut changes = rtc.sdp_api();
            changes.add_channel("horizon-nearby".to_owned());
            let Some((offer, offer_pending)) = changes.apply() else {
                finish(
                    &mut rtc,
                    &socket,
                    &channel,
                    &events,
                    Some("the offer needed no negotiation".to_owned()),
                );
                return;
            };
            pending = Some(offer_pending);
            match serde_json::to_string(&offer) {
                Ok(json) => emit(&events, Event::LocalOffer(json)),
                Err(e) => {
                    finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                    return;
                }
            }
        }
    }

    let mut buf = vec![0u8; 2048];
    let mut out: VecDeque<Vec<u8>> = VecDeque::new();
    loop {
        // Drain str0m's output to the next timeout. Every mutation below is
        // followed by this drain, as str0m requires.
        let next = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    if let Err(e) = socket.send_to(&t.contents, t.destination) {
                        warn!("horizon-webrtc: UDP send failed: {e}");
                    }
                }
                Ok(Output::Event(event)) => match event {
                    RtcEvent::ChannelOpen(id, label) => {
                        debug!("horizon-webrtc: channel {label:?} open");
                        cid = Some(id);
                        emit(&events, Event::Connected);
                    }
                    RtcEvent::ChannelData(data) if !channel.inner.receive(&data.data) => {
                        finish(
                            &mut rtc,
                            &socket,
                            &channel,
                            &events,
                            Some("the peer sends faster than the game reads".to_owned()),
                        );
                        return;
                    }
                    RtcEvent::ChannelData(_) => {}
                    RtcEvent::ChannelClose(_) | RtcEvent::Closed => {
                        finish(&mut rtc, &socket, &channel, &events, None);
                        return;
                    }
                    RtcEvent::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        finish(
                            &mut rtc,
                            &socket,
                            &channel,
                            &events,
                            Some("the WebRTC link was lost".to_owned()),
                        );
                        return;
                    }
                    _ => {}
                },
                Err(e) => {
                    finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                    return;
                }
            }
        };

        // One message at a time, cut into data-channel-sized pieces. A full
        // SCTP buffer keeps the piece for the next pass.
        if out.is_empty()
            && let Some(message) = channel.inner.take()
        {
            for piece in message.chunks(16 * 1024) {
                out.push_back(piece.to_vec());
            }
        }
        if let Some(id) = cid {
            while let Some(piece) = out.pop_front() {
                let Some(mut data) = rtc.channel(id) else {
                    out.push_front(piece);
                    break;
                };
                match data.write(true, &piece) {
                    Ok(true) => {}
                    Ok(false) => {
                        out.push_front(piece);
                        break;
                    }
                    Err(e) => {
                        finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                        return;
                    }
                }
            }
        }
        if channel.inner.is_closed() {
            finish(&mut rtc, &socket, &channel, &events, None);
            return;
        }

        // One command, if any. `Wake` is only a nudge.
        let mut stop = false;
        loop {
            match commands.try_recv() {
                Ok(Command::Wake) => {}
                Ok(Command::Close) => {
                    stop = true;
                    break;
                }
                Ok(Command::AcceptAnswer(json)) => {
                    let Some(offer_pending) = pending.take() else {
                        continue;
                    };
                    let json = crate::sdp::rewrite_mdns(&json).unwrap_or(json);
                    match serde_json::from_str::<SdpAnswer>(&json) {
                        Ok(answer) => {
                            if let Err(e) = rtc.sdp_api().accept_answer(offer_pending, answer) {
                                finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                                return;
                            }
                        }
                        Err(e) => {
                            finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                            return;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stop = true;
                    break;
                }
            }
        }
        if stop {
            finish(&mut rtc, &socket, &channel, &events, None);
            return;
        }

        // Wait for one thing: a UDP packet, str0m's timeout, or a command.
        let now = Instant::now();
        let left = next
            .saturating_duration_since(now)
            .clamp(Duration::from_millis(1), Duration::from_millis(10));
        let _ = socket.set_read_timeout(Some(left));
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                let destination = socket
                    .local_addr()
                    .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
                if let Ok(contents) = Receive::new(Protocol::Udp, source, destination, &buf[..n])
                    && let Err(e) = rtc.handle_input(Input::Receive(Instant::now(), contents))
                {
                    finish(&mut rtc, &socket, &channel, &events, Some(e.to_string()));
                    return;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let _ = rtc.handle_input(Input::Timeout(Instant::now()));
            }
            Err(e) => {
                warn!("horizon-webrtc: UDP receive failed: {e}");
                let _ = rtc.handle_input(Input::Timeout(Instant::now()));
            }
        }
    }
}

fn finish(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    channel: &Channel,
    events: &Sender<Event>,
    reason: Option<String>,
) {
    let _ = rtc.close();
    // Let the close_notify and SCTP shutdown reach the socket.
    for _ in 0..8 {
        match rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                let _ = socket.send_to(&t.contents, t.destination);
            }
            Ok(Output::Event(_)) => {}
            Ok(Output::Timeout(_)) | Err(_) => break,
        }
    }
    channel.inner.close();
    emit(events, Event::Closed(reason));
}
