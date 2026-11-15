//! Play Nearby: discovery and WebRTC sessions for a local world.
//!
//! The pieces:
//! - [`proto`]: presence, signalling and their sealed envelopes.
//! - [`room`]: the hash that puts nearby clients on the same broker topic.
//! - [`mqtt`]: a minimal MQTT 3.1.1 client for the public broker pool.
//! - [`lan`]: richer presence and chunked signalling on the local network.
//! - [`pump`]: the friends tunnel's secure records over a plain stream.
//! - [`service`]: the state machine the game drives once a frame.
pub mod broker;
pub mod lan;
pub mod mqtt;
pub mod proto;
pub mod pump;
pub mod room;
pub mod service;
pub mod session;
mod transport;

pub use proto::Policy;
pub use service::{Command, Config, Event, Service, Snapshot};
