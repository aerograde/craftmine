//! Presence, signalling messages and their sealed envelopes.
//!
//! Presence is public by design: a random id, display names, the world's
//! shape and an X25519 public key. Signalling is sealed to the recipient's
//! key (or to a per-session key), so a broker relays opaque blobs. The game
//! stream has its own SPAKE2 tunnel keyed by the accepted token.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{fmt, io};
use x25519_dalek::{PublicKey, StaticSecret};

/// The nearby wire version; the MQTT topics carry it too.
pub const VERSION: u8 = 1;
/// The most bytes of a sealed signalling message.
pub const MAX_ENVELOPE: usize = 64 * 1024;
const NONCE: usize = 12;
const KEY: usize = 32;

/// Who may join a shared world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    Ask,
    Open,
    InviteOnly,
}
impl Policy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask to join",
            Self::Open => "Anyone nearby",
            Self::InviteOnly => "Invite only",
        }
    }
}

/// What a player is doing, for the people list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerStatus {
    #[default]
    Menu,
    Playing,
    Hosting,
}

/// What a presence record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceKind {
    World,
    Player,
}

/// One announced world or player.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub v: u8,
    pub kind: PresenceKind,
    /// 16 hex, random per app session.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub players: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
    #[serde(default)]
    pub status: PlayerStatus,
    /// Base64 X25519 static public key, for sealing to this peer.
    pub key: String,
    pub proto: i32,
    pub game: String,
    pub room: String,
    /// Unix milliseconds.
    pub ts: u64,
}

impl Presence {
    /// Shrink and reject a record from the network before it reaches the UI.
    pub fn sanitize(&mut self) -> bool {
        if self.v != VERSION || !valid_id(&self.id) || self.key.len() > 64 {
            return false;
        }
        truncate(&mut self.name, 64);
        truncate(&mut self.game, 32);
        truncate(&mut self.room, 40);
        if self.name.is_empty() {
            return false;
        }
        for id in [&self.host_id, &self.world_id].into_iter().flatten() {
            if !valid_id(id) {
                return false;
            }
        }
        true
    }
}

/// A nearby world's row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub world_id: String,
    pub guest_id: String,
    pub guest_name: String,
    /// Base64 X25519 ephemeral public key for the per-session key.
    pub eph: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// The host's answer to a join request; the token keys the data-channel
/// SPAKE2 exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinAccepted {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub world_id: String,
    pub host_id: String,
    pub host_eph: String,
    pub token: String,
    pub session: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub session: u64,
    pub sdp: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub session: u64,
    pub sdp: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    Full,
    InviteOnly,
    Version,
    Denied,
    Unreachable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinDenied {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub world_id: String,
    pub reason: DenyReason,
}

/// A host's invitation to a nearby player.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub host_id: String,
    pub host_name: String,
    pub world_id: String,
    pub world_name: String,
    pub token: String,
    /// The host's static key, so an invitee can seal its request without
    /// waiting for a presence refresh.
    pub host_key: String,
    pub expires: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    pub v: u8,
    pub msg_id: String,
    pub ts: u64,
    pub id: String,
}

/// One signalling message in a sealed envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Signal {
    JoinRequest(JoinRequest),
    JoinAccepted(JoinAccepted),
    Offer(Offer),
    Answer(Answer),
    JoinDenied(JoinDenied),
    Invite(Invite),
    Bye(Bye),
}
impl Signal {
    pub fn msg_id(&self) -> &str {
        match self {
            Self::JoinRequest(m) => &m.msg_id,
            Self::JoinAccepted(m) => &m.msg_id,
            Self::Offer(m) => &m.msg_id,
            Self::Answer(m) => &m.msg_id,
            Self::JoinDenied(m) => &m.msg_id,
            Self::Invite(m) => &m.msg_id,
            Self::Bye(m) => &m.msg_id,
        }
    }
    pub fn version(&self) -> u8 {
        match self {
            Self::JoinRequest(m) => m.v,
            Self::JoinAccepted(m) => m.v,
            Self::Offer(m) => m.v,
            Self::Answer(m) => m.v,
            Self::JoinDenied(m) => m.v,
            Self::Invite(m) => m.v,
            Self::Bye(m) => m.v,
        }
    }
}

/// A sealed signalling payload. `eph` is set on a message sealed to the
/// recipient's static key; `session` on one sealed with the session key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub eph: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<u64>,
    pub nonce: String,
    pub ct: String,
}

/// A long-lived X25519 key for sealing to a peer by its presence key.
pub struct StaticKey(StaticSecret);
impl StaticKey {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; KEY];
        random_fill(&mut bytes)?;
        Ok(Self(StaticSecret::from(bytes)))
    }
    pub fn public(&self) -> String {
        B64.encode(PublicKey::from(&self.0).to_bytes())
    }
    /// Seal `signal` to `to_public`, an ephemeral key per message.
    pub fn seal(
        &self,
        from: &str,
        to: &str,
        to_public: &str,
        signal: &Signal,
    ) -> io::Result<Envelope> {
        let recipient = public_key(to_public)?;
        let mut secret = [0u8; KEY];
        random_fill(&mut secret)?;
        let ephemeral = StaticSecret::from(secret);
        let shared = ephemeral.diffie_hellman(&recipient);
        let key = hkdf(shared.as_bytes(), b"horizon-nearby v1 seal");
        let aad = aad(VERSION, from, to, None);
        let (nonce, ct) = encrypt(&key, &aad, signal)?;
        Ok(Envelope {
            v: VERSION,
            from: from.to_owned(),
            to: to.to_owned(),
            eph: B64.encode(PublicKey::from(&ephemeral).to_bytes()),
            session: None,
            nonce,
            ct,
        })
    }
    /// Open a message sealed to this key.
    pub fn open(&self, envelope: &Envelope) -> io::Result<Signal> {
        let ephemeral = public_key(&envelope.eph)?;
        let shared = self.0.diffie_hellman(&ephemeral);
        let key = hkdf(shared.as_bytes(), b"horizon-nearby v1 seal");
        let aad = aad(envelope.v, &envelope.from, &envelope.to, envelope.session);
        decrypt(&key, &aad, envelope)
    }
    /// The per-session key from this side's ephemeral secret and the peer's
    /// ephemeral public key.
    pub fn session(&self, peer_eph: &str) -> io::Result<SessionKey> {
        let peer = public_key(peer_eph)?;
        let shared = self.0.diffie_hellman(&peer);
        Ok(SessionKey(hkdf(
            shared.as_bytes(),
            b"horizon-nearby v1 session",
        )))
    }
}

/// A per-session key for offers, answers and further signalling.
#[derive(Clone)]
pub struct SessionKey([u8; KEY]);
impl SessionKey {
    pub fn seal(
        &self,
        from: &str,
        to: &str,
        session: u64,
        signal: &Signal,
    ) -> io::Result<Envelope> {
        let aad = aad(VERSION, from, to, Some(session));
        let (nonce, ct) = encrypt(&self.0, &aad, signal)?;
        Ok(Envelope {
            v: VERSION,
            from: from.to_owned(),
            to: to.to_owned(),
            eph: String::new(),
            session: Some(session),
            nonce,
            ct,
        })
    }
    pub fn open(&self, envelope: &Envelope) -> io::Result<Signal> {
        let aad = aad(envelope.v, &envelope.from, &envelope.to, envelope.session);
        decrypt(&self.0, &aad, envelope)
    }
}

/// Fill a buffer with the browser's or the OS's randomness.
pub fn random_fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(|e| io::Error::other(e.to_string()))
}

/// A new 16-hex id (8 random bytes).
pub fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 8];
    random_fill(&mut bytes)?;
    Ok(hex(&bytes))
}

/// A new 32-hex one-time token (16 random bytes).
pub fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    random_fill(&mut bytes)?;
    Ok(hex(&bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len * 2 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn now_ms() -> u64 {
    use horizon_os::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn truncate(text: &mut String, max: usize) {
    if text.chars().count() > max {
        *text = text.chars().take(max).collect();
    }
    text.retain(|c| !c.is_control());
}

fn public_key(encoded: &str) -> io::Result<PublicKey> {
    if encoded.len() > 64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "key too long"));
    }
    let bytes = B64
        .decode(encoded)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad key"))?;
    let bytes: [u8; KEY] = bytes
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad key length"))?;
    Ok(PublicKey::from(bytes))
}

fn hkdf(shared: &[u8], info: &[u8]) -> [u8; KEY] {
    let mut out = [0u8; KEY];
    Hkdf::<Sha256>::new(None, shared)
        .expand(info, &mut out)
        .expect("32 bytes is a valid HKDF output");
    out
}

fn aad(v: u8, from: &str, to: &str, session: Option<u64>) -> Vec<u8> {
    format!("{v}|{from}|{to}|{}", session.unwrap_or(0)).into_bytes()
}

fn encrypt(key: &[u8; KEY], aad: &[u8], signal: &Signal) -> io::Result<(String, String)> {
    let plain = serde_json::to_vec(signal).map_err(io::Error::other)?;
    let mut nonce = [0u8; NONCE];
    random_fill(&mut nonce)?;
    let cipher = ChaCha20Poly1305::new(key.into());
    let ct = cipher
        .encrypt(&nonce.into(), Payload { msg: &plain, aad })
        .map_err(|_| io::Error::other("sealing failed"))?;
    Ok((B64.encode(nonce), B64.encode(ct)))
}

fn decrypt(key: &[u8; KEY], aad: &[u8], envelope: &Envelope) -> io::Result<Signal> {
    if envelope.ct.len() > MAX_ENVELOPE || envelope.nonce.len() > 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "envelope too big",
        ));
    }
    let nonce = B64
        .decode(&envelope.nonce)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad nonce"))?;
    let nonce: [u8; NONCE] = nonce
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad nonce length"))?;
    let ct = B64
        .decode(&envelope.ct)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad ciphertext"))?;
    let cipher = ChaCha20Poly1305::new(key.into());
    let plain = cipher
        .decrypt(&nonce.into(), Payload { msg: &ct, aad })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the envelope did not open"))?;
    let signal: Signal = serde_json::from_slice(&plain)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if signal.version() != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "wrong wire version",
        ));
    }
    Ok(signal)
}

impl fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StaticKey").field(&self.public()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> JoinRequest {
        JoinRequest {
            v: VERSION,
            msg_id: random_id().unwrap(),
            ts: now_ms(),
            world_id: random_id().unwrap(),
            guest_id: random_id().unwrap(),
            guest_name: "Steve".into(),
            eph: B64.encode([9u8; 32]),
            token: None,
        }
    }

    #[test]
    fn static_sealing_round_trips_and_rejects_another_key() {
        let host = StaticKey::generate().unwrap();
        let other = StaticKey::generate().unwrap();
        let envelope = host
            .seal(
                &request().guest_id,
                &random_id().unwrap(),
                &host.public(),
                &Signal::JoinRequest(request()),
            )
            .unwrap();
        let opened = host.open(&envelope).unwrap();
        assert!(matches!(opened, Signal::JoinRequest(_)));
        assert!(other.open(&envelope).is_err());
        let mut tampered = envelope.clone();
        tampered.ct.push('A');
        assert!(host.open(&tampered).is_err());
    }

    #[test]
    fn session_sealing_round_trips() {
        let guest_eph = StaticKey::generate().unwrap();
        let host_eph = StaticKey::generate().unwrap();
        let from_guest = guest_eph.session(&host_eph.public()).unwrap();
        let from_host = host_eph.session(&guest_eph.public()).unwrap();
        let msg = Signal::Offer(Offer {
            v: VERSION,
            msg_id: random_id().unwrap(),
            ts: now_ms(),
            session: 7,
            sdp: "v=0".into(),
        });
        let envelope = from_guest.seal("a", "b", 7, &msg).unwrap();
        assert_eq!(from_host.open(&envelope).unwrap(), msg);
    }

    #[test]
    fn ids_and_tokens_are_checked() {
        assert!(valid_id(&random_id().unwrap()));
        assert!(!valid_id("nope"));
        assert!(valid_hex(&random_token().unwrap(), 16));
        let mut presence = Presence {
            v: VERSION,
            kind: PresenceKind::Player,
            id: random_id().unwrap(),
            name: "x".repeat(200),
            host_id: None,
            world_id: None,
            players: None,
            max: None,
            policy: None,
            status: PlayerStatus::Menu,
            key: "k".into(),
            proto: 777,
            game: "26.3".into(),
            room: "auto-1".into(),
            ts: now_ms(),
        };
        assert!(presence.sanitize());
        assert_eq!(presence.name.chars().count(), 64);
    }
}
