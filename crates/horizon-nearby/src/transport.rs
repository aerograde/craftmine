//! The WebRTC peer behind one small type: the native `str0m` wrapper, or the
//! browser's `RTCPeerConnection` through `horizon-os`.
use crate::{Config, pump::Secure};
use horizon_os::time::Duration;
use std::io::{self, Read, Write};

#[derive(Debug)]
pub enum PeerEvent {
    LocalOffer(String),
    LocalAnswer(String),
    Connected,
    Closed(Option<String>),
}

/// One peer connection. It owns a thread; the session thread polls it.
pub struct Peer {
    inner: Inner,
}

enum Inner {
    #[cfg(not(target_arch = "wasm32"))]
    Native(horizon_webrtc::Peer),
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    Web(web::Peer),
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    Disabled,
}

#[cfg(not(target_arch = "wasm32"))]
fn inner_offer(config: &Config) -> io::Result<Inner> {
    Ok(Inner::Native(horizon_webrtc::Peer::offer(webrtc_config(
        config,
    ))?))
}
#[cfg(not(target_arch = "wasm32"))]
fn inner_answer(config: &Config, offer: String) -> io::Result<Inner> {
    Ok(Inner::Native(horizon_webrtc::Peer::answer(
        webrtc_config(config),
        offer,
    )?))
}
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn inner_offer(config: &Config) -> io::Result<Inner> {
    Ok(Inner::Web(web::Peer::offer(config)?))
}
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn inner_answer(config: &Config, offer: String) -> io::Result<Inner> {
    Ok(Inner::Web(web::Peer::answer(config, offer)?))
}
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn inner_offer(_config: &Config) -> io::Result<Inner> {
    Err(unsupported())
}
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn inner_answer(_config: &Config, _offer: String) -> io::Result<Inner> {
    Err(unsupported())
}

impl Peer {
    pub fn offer(config: &Config) -> io::Result<Self> {
        Ok(Self {
            inner: inner_offer(config)?,
        })
    }

    pub fn answer(config: &Config, offer: String) -> io::Result<Self> {
        Ok(Self {
            inner: inner_answer(config, offer)?,
        })
    }

    pub fn poll(&mut self) -> Vec<PeerEvent> {
        match &mut self.inner {
            #[cfg(not(target_arch = "wasm32"))]
            Inner::Native(peer) => peer
                .poll()
                .into_iter()
                .map(|event| match event {
                    horizon_webrtc::Event::LocalOffer(sdp) => PeerEvent::LocalOffer(sdp),
                    horizon_webrtc::Event::LocalAnswer(sdp) => PeerEvent::LocalAnswer(sdp),
                    horizon_webrtc::Event::Connected => PeerEvent::Connected,
                    horizon_webrtc::Event::Data(_) => {
                        PeerEvent::Closed(Some("an unexpected data event".to_owned()))
                    }
                    horizon_webrtc::Event::Closed(reason) => PeerEvent::Closed(reason),
                })
                .collect(),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Inner::Web(peer) => peer.poll(),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Inner::Disabled => Vec::new(),
        }
    }

    pub fn accept_answer(&self, answer: String) {
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
            Inner::Native(peer) => peer.accept_answer(answer),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Inner::Web(peer) => peer.accept_answer(answer),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Inner::Disabled => {}
        }
    }

    pub fn channel(&mut self) -> io::Result<Link> {
        match &mut self.inner {
            #[cfg(not(target_arch = "wasm32"))]
            Inner::Native(peer) => Ok(Link::Native(peer.channel())),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Inner::Web(peer) => Ok(Link::Web(peer.channel()?)),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Inner::Disabled => Err(unsupported()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn webrtc_config(config: &Config) -> horizon_webrtc::Config {
    horizon_webrtc::Config {
        stun: config.stun.clone(),
        gather: config.ice_timeout.min(Duration::from_secs(3)),
        connect: config.connect_timeout,
        loopback: config.loopback,
    }
}

/// The data channel as a byte stream.
pub enum Link {
    #[cfg(not(target_arch = "wasm32"))]
    Native(horizon_webrtc::Channel),
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    Web(horizon_os::net::WebSocket),
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    Disabled,
}

impl Link {
    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => Ok(Self::Native(channel.try_clone())),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => Ok(Self::Web(socket.try_clone())),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => Err(unsupported()),
        }
    }
}

impl Read for Link {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => channel.read(out),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => socket.read(out),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => Ok(0),
        }
    }
}
impl Write for Link {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => channel.write(data),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => socket.write(data),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => channel.flush(),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => socket.flush(),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => Ok(()),
        }
    }
}

impl Secure for Link {
    fn set_secure_timeout(&self, timeout: Option<Duration>) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => channel.set_read_timeout(timeout),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => horizon_os::net::WebSocket::set_read_timeout(socket, timeout),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => {}
        }
    }
    fn shutdown_secure(&self) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Native(channel) => channel.shutdown(),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(socket) => horizon_os::net::WebSocket::shutdown(socket),
            #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
            Self::Disabled => {}
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "WebRTC is unavailable in this build",
    )
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
mod web {
    //! The browser's own `RTCPeerConnection`, driven by `horizon-os`.
    use crate::{Config, transport::PeerEvent};
    use horizon_os::net::{Peer as BrowserPeer, WebSocket};
    use std::io;

    pub struct Peer {
        inner: BrowserPeer,
        channel: Option<WebSocket>,
    }

    fn ice(config: &Config) -> Vec<String> {
        config
            .stun
            .iter()
            .filter(|server| !server.starts_with("turn:") && !server.starts_with("turns:"))
            .map(|server| {
                if server.starts_with("stun:") {
                    server.clone()
                } else {
                    format!("stun:{server}")
                }
            })
            .collect()
    }

    impl Peer {
        pub fn offer(config: &Config) -> io::Result<Self> {
            let inner = BrowserPeer::offer(&ice(config), "horizon-nearby", config.connect_timeout)?;
            Ok(Self {
                inner,
                channel: None,
            })
        }
        pub fn answer(config: &Config, offer: String) -> io::Result<Self> {
            let inner = BrowserPeer::answer(
                &ice(config),
                &offer,
                "horizon-nearby",
                config.connect_timeout,
            )?;
            Ok(Self {
                inner,
                channel: None,
            })
        }
        pub fn poll(&mut self) -> Vec<PeerEvent> {
            let mut events = Vec::new();
            if let Some(offer) = self.inner.take_offer() {
                events.push(PeerEvent::LocalOffer(offer));
            }
            if let Some(answer) = self.inner.take_answer() {
                events.push(PeerEvent::LocalAnswer(answer));
            }
            match self.inner.take_channel() {
                Ok(Some(channel)) => {
                    self.channel = Some(channel);
                    events.push(PeerEvent::Connected);
                }
                Ok(None) => {}
                Err(e) => events.push(PeerEvent::Closed(Some(e.to_string()))),
            }
            if let Some(error) = self.inner.take_error() {
                events.push(PeerEvent::Closed(Some(error)));
            }
            events
        }
        pub fn accept_answer(&self, answer: String) {
            self.inner.accept_answer(&answer);
        }
        pub fn channel(&mut self) -> io::Result<WebSocket> {
            self.channel.take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "the data channel is not open")
            })
        }
    }
}
