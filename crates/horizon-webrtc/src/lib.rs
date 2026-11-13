//! A WebRTC data channel as a blocking byte stream.
//!
//! `str0m` is sans-I/O: it never touches a socket or a clock. [`Peer`] owns one
//! `str0m::Rtc` on its own thread, drives the UDP socket, and exposes the data
//! channel as a [`Channel`] that implements `Read` and `Write`. The rest of the
//! game only sees bytes, so a WebRTC link slots under the same pump as the
//! friends tunnel's TCP links.
use horizon_os::time::Duration;

mod channel;
mod mdns;
mod peer;
mod sdp;

pub use channel::Channel;
pub use peer::{Command, Event, Peer};

/// How a peer gathers and connects.
#[derive(Clone, Debug)]
pub struct Config {
    /// STUN servers (`host:port`), asked on the peer's own UDP socket.
    pub stun: Vec<String>,
    /// How long to wait for gathering and then for the channel.
    pub gather: Duration,
    pub connect: Duration,
    /// Add `127.0.0.1` host candidates: two peers of one process (tests).
    pub loopback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stun: crate::default_stun(),
            gather: Duration::from_secs(3),
            connect: Duration::from_secs(20),
            loopback: false,
        }
    }
}

/// Google first, then Cloudflare, as the game's other NAT traversal does.
pub fn default_stun() -> Vec<String> {
    [
        "stun.l.google.com:19302",
        "stun1.l.google.com:19302",
        "stun2.l.google.com:19302",
        "stun.cloudflare.com:3478",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// The one crypto provider `str0m` uses. `install_process_default` may only
/// run once, and a peer can be started from several threads.
pub(crate) fn install_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        str0m::crypto::from_feature_flags().install_process_default();
        log::debug!("horizon-webrtc: str0m crypto provider installed");
    });
}
