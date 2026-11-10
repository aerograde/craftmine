//! A host's end of an Eaglercraft connection: the login handshake a guest runs
//! against a shared world, mirroring the client in [`crate::eagler`]. The
//! plugin logs in to the backend for the guest, so this host plays the
//! plugin's part: it answers `CLIENT_VERSION` with `SERVER_VERSION`, gives the
//! name and offline UUID back with `ALLOW_LOGIN`, takes the guest's skin and
//! cape as `PROFILE_DATA`, and finishes with `LOGIN_COMPLETE`. Every message
//! is one binary message, never framed, compressed or encrypted.
//!
//! A guest whose Minecraft protocol this world does not speak is refused with
//! a `VERSION_MISMATCH` listing the protocol the world runs. Protocol 47
//! (1.8) is spoken through [`EaglerServerSocket::legacy`], which translates
//! the world's packets; other versions wait for their own codec
//! ([`crate::translate`]).
use crate::{
    PROTOCOL_VERSION,
    buf::{Reader, Writer},
    crypto::offline_uuid,
    eagler::{Message, Messages},
    frame,
    packets::{handshake, login},
    translate::{
        legacy::{
            LegacyState, PROTOCOL_1_8, packets as legacy_packets,
            play::{GuestTranslator, PlayTranslator},
        },
        modern::{
            PROTOCOL_26_2, rewrite_clientbound_configuration, rewrite_clientbound_play,
            rewrite_serverbound_play,
        },
    },
};
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex, MutexGuard},
};

/// The Eagler protocol this host speaks: 5, as the client offers.
const EAGLER_PROTOCOL: u16 = 5;
/// A skin and a cape are the only entries a client sends; a few more are
/// harmless, a flood is not.
const MAX_PROFILE_ENTRIES: usize = 8;
const MAX_PROFILE_ENTRY: usize = 1 << 20;
/// The most protocols a client may offer in one handshake packet.
const MAX_PROTOCOLS: u16 = 64;

/// What the host does with one guest message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAction {
    /// Login messages to send to the guest, in order.
    Handshake(Vec<Vec<u8>>),
    /// A game packet from the logged-in guest: one binary message, no length
    /// prefix, for the world's server.
    Play(Vec<u8>),
    /// An `0xEE` plugin-channel message from the guest, without its id byte.
    Plugin(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for CLIENT_VERSION.
    Version,
    /// Waiting for REQUEST_LOGIN.
    Login,
    /// Waiting for PROFILE_DATA and FINISH_LOGIN.
    Finish,
    Play,
    /// The login was refused; nothing more is read.
    Closed,
}

pub struct EaglerHost {
    phase: Phase,
    /// The game protocol this host speaks and accepted.
    protocol: u16,
    /// What this host says it is (`SERVER_VERSION`).
    brand: String,
    version: String,
    /// What the guest said it is (`CLIENT_VERSION`).
    client_brand: String,
    client_version: String,
    name: String,
    uuid: u128,
    profile: Vec<(String, Vec<u8>)>,
}

impl EaglerHost {
    /// A host speaking this game's protocol.
    pub fn new(brand: &str, version: &str) -> Self {
        Self::speaking(
            brand,
            version,
            u16::try_from(PROTOCOL_VERSION).unwrap_or(u16::MAX),
        )
    }

    /// A host speaking `protocol`, for a guest translated to another version.
    pub fn speaking(brand: &str, version: &str, protocol: u16) -> Self {
        Self {
            phase: Phase::Version,
            protocol,
            brand: brand.to_owned(),
            version: version.to_owned(),
            client_brand: String::new(),
            client_version: String::new(),
            name: String::new(),
            uuid: 0,
            profile: Vec::new(),
        }
    }

    /// Handles one binary message from the guest. `Err` means the connection
    /// should be closed: the guest sent something the handshake never does.
    pub fn message(&mut self, data: &[u8]) -> Result<HostAction, String> {
        if self.phase == Phase::Play {
            return Ok(match data.split_first() {
                Some((0xEE, rest)) => HostAction::Plugin(rest.to_vec()),
                _ => HostAction::Play(data.to_vec()),
            });
        }
        if self.phase == Phase::Closed {
            return Err("The guest's login was refused.".into());
        }
        let mut r = Reader::new(data);
        let id = r.u8().map_err(truncated)?;
        match (self.phase, id) {
            (Phase::Version, 0x01) => self.client_version(&mut r),
            (Phase::Login, 0x04) => self.request_login(&mut r),
            (Phase::Finish, 0x07) => self.profile_data(&mut r),
            (Phase::Finish, 0x08) => {
                self.phase = Phase::Play;
                Ok(HostAction::Handshake(vec![vec![0x09]]))
            }
            _ => Err("Unexpected Eaglercraft handshake message.".into()),
        }
    }

    pub fn is_playing(&self) -> bool {
        self.phase == Phase::Play
    }

    /// The guest's name and UUID, once it plays.
    pub fn player(&self) -> Option<(&str, u128)> {
        (self.phase == Phase::Play).then_some((&self.name, self.uuid))
    }

    /// The guest's `PROFILE_DATA` entries (`skin_v2`, `cape_v1`), unpacked.
    pub fn profile(&self) -> &[(String, Vec<u8>)] {
        &self.profile
    }

    /// The game protocol the guest and this host agreed on.
    pub fn protocol(&self) -> u16 {
        self.protocol
    }

    /// The client's brand and version, from `CLIENT_VERSION`.
    pub fn client(&self) -> (&str, &str) {
        (&self.client_brand, &self.client_version)
    }

    /// `CLIENT_VERSION`: the legacy marker, the Eagler and Minecraft
    /// protocols offered, the client's brand and version, its password (empty
    /// for a world) and its name.
    fn client_version(&mut self, r: &mut Reader) -> Result<HostAction, String> {
        let _legacy = r.u8().map_err(truncated)?;
        let eagler = protocol_list(r)?;
        let minecraft = protocol_list(r)?;
        self.client_brand = str8(r)?;
        self.client_version = str8(r)?;
        let _password = str8(r)?;
        self.name = str8(r)?;
        if !eagler.contains(&EAGLER_PROTOCOL) {
            self.phase = Phase::Closed;
            return Ok(HostAction::Handshake(vec![version_mismatch(self.protocol)]));
        }
        // A 26.2 client offers 776 beside this world's 777. Accept it and
        // advertise 776; the play connection rewrites the three packet-id
        // differences through `translate::modern`.
        if !minecraft.contains(&self.protocol) {
            let backend = u16::try_from(PROTOCOL_VERSION).unwrap_or(u16::MAX);
            if self.protocol == backend && minecraft.contains(&(PROTOCOL_26_2 as u16)) {
                self.protocol = PROTOCOL_26_2 as u16;
            } else {
                self.phase = Phase::Closed;
                return Ok(HostAction::Handshake(vec![version_mismatch(self.protocol)]));
            }
        }
        self.phase = Phase::Login;
        Ok(HostAction::Handshake(vec![self.server_version()]))
    }

    /// `REQUEST_LOGIN`: the name (and the server, password and capabilities a
    /// shared world does not use).
    fn request_login(&mut self, r: &mut Reader) -> Result<HostAction, String> {
        let name = str8(r)?;
        let _server = str8(r)?;
        if name.trim().is_empty() {
            self.phase = Phase::Closed;
            return Ok(HostAction::Handshake(vec![deny(
                "A name is required to join.",
            )]));
        }
        self.name = name;
        self.uuid = offline_uuid(&self.name);
        self.phase = Phase::Finish;
        Ok(HostAction::Handshake(vec![allow_login(
            &self.name, self.uuid,
        )]))
    }

    /// `PROFILE_DATA`: the guest's skin and cape, kept for the other players.
    fn profile_data(&mut self, r: &mut Reader) -> Result<HostAction, String> {
        let count = usize::from(r.u8().map_err(truncated)?);
        if count > MAX_PROFILE_ENTRIES {
            return Err("The guest sent too many profile entries.".into());
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = str8(r)?;
            let len = usize::from(r.u16().map_err(truncated)?);
            if len > MAX_PROFILE_ENTRY {
                return Err("A profile entry is too large.".into());
            }
            let data = r.bytes(len).map_err(truncated)?.to_vec();
            entries.push((kind, data));
        }
        self.profile = entries;
        Ok(HostAction::Handshake(Vec::new()))
    }

    /// `SERVER_VERSION`, in the shape the plugin sends: the chosen Eagler and
    /// Minecraft protocols, the host's brand and version, then no auth, no
    /// salt and no nickname selection.
    fn server_version(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x02);
        w.u16(EAGLER_PROTOCOL);
        w.u16(self.protocol);
        str8_write(&mut w, &self.brand);
        str8_write(&mut w, &self.version);
        // No auth, no salt; a 26.2 client must be asked for its name, or it
        // sends REQUEST_LOGIN with an empty one and is refused.
        let nickname = u8::from(self.protocol == PROTOCOL_26_2 as u16);
        w.data.extend_from_slice(&[0, 0, 0, nickname]);
        w.data
    }
}

/// `ALLOW_LOGIN`: the name the world gives the guest and its offline UUID,
/// then the two flags the plugin sends.
fn allow_login(name: &str, uuid: u128) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(0x05);
    str8_write(&mut w, name);
    w.data.extend_from_slice(&uuid.to_be_bytes());
    w.data.extend_from_slice(&[0, 0]);
    w.data
}

/// `DENY_LOGIN`: a JSON text component.
fn deny(reason: &str) -> Vec<u8> {
    let reason = serde_json::json!({ "text": reason }).to_string();
    let mut w = Writer::new();
    w.u8(0x06);
    w.u16(reason.len() as u16);
    w.data.extend_from_slice(reason.as_bytes());
    w.data
}

/// `VERSION_MISMATCH`: the Eagler protocols this host speaks, the Minecraft
/// range it accepts, then a message.
fn version_mismatch(protocol: u16) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(0x03);
    w.u16(1);
    w.u16(EAGLER_PROTOCOL);
    // The field between the lists and the range, which clients skip.
    w.u16(0);
    w.u16(protocol);
    w.u16(protocol);
    str8_write(
        &mut w,
        &format!("This world speaks Minecraft protocol {protocol}."),
    );
    w.data
}

fn truncated(_: crate::Error) -> String {
    "The guest's handshake is truncated.".into()
}

fn str8(r: &mut Reader) -> Result<String, String> {
    let len = usize::from(r.u8().map_err(truncated)?);
    let bytes = r.bytes(len).map_err(truncated)?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn str8_write(w: &mut Writer, text: &str) {
    let bytes = &text.as_bytes()[..text.len().min(255)];
    w.u8(bytes.len() as u8);
    w.data.extend_from_slice(bytes);
}

/// The Minecraft protocols a guest's `CLIENT_VERSION` offers, for choosing
/// its translation.
pub fn client_protocols(body: &[u8]) -> Option<Vec<u16>> {
    let mut r = Reader::new(body);
    if r.u8().ok()? != 0x01 {
        return None;
    }
    let _legacy = r.u8().ok()?;
    let _eagler = protocol_list(&mut r).ok()?;
    protocol_list(&mut r).ok()
}

fn protocol_list(r: &mut Reader) -> Result<Vec<u16>, String> {
    let count = r.u16().map_err(truncated)?;
    if count > MAX_PROTOCOLS {
        return Err("The guest offered too many protocols.".into());
    }
    (0..count)
        .map(|_| r.u16().map_err(truncated))
        .collect::<Result<Vec<_>, _>>()
}

/// A shared world's end of an Eaglercraft connection: the server counterpart
/// of [`crate::eagler::EaglerSocket`]. It runs the plugin handshake, then
/// presents the guest to the world's login exactly as the plugin does: a
/// handshake and a login hello for the name and UUID the host gave, the
/// plugin's own login acknowledgement (a guest never sends the backend's),
/// and from then on game packets as messages, decompressed and recompressed
/// around the backend's threshold.
pub struct EaglerServerSocket {
    socket: Box<dyn Messages>,
    shared: Arc<Mutex<ServerShared>>,
    outgoing: Vec<u8>,
}

struct ServerShared {
    host: EaglerHost,
    phase: ServerPhase,
    /// Framed bytes for the world's connection to read.
    incoming: Vec<u8>,
    /// The backend's compression threshold, once its login sent it.
    threshold: Option<usize>,
    /// A guest older than the world's protocol (1.8), with its translators.
    legacy: Option<LegacyGuest>,
    /// A modern guest has not yet acknowledged the forwarded configuration
    /// finish; its configuration messages are dropped until it does.
    guest_config: bool,
}

struct LegacyGuest {
    play: PlayTranslator,
    guest: GuestTranslator,
    /// A modern block state as 1.8's id and metadata.
    resolve: Arc<dyn Fn(u32) -> LegacyState + Send + Sync>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServerPhase {
    /// The Eagler plugin handshake is running.
    Hosting,
    /// The synthesized handshake and hello are in; the backend's login is.
    Backend,
    /// A 1.8 guest is logged in, and the backend's configuration state is
    /// being discarded (1.8 has none).
    Configuration,
    Play,
    Closed,
}

impl EaglerServerSocket {
    pub fn new(socket: Box<dyn Messages>, brand: &str, version: &str) -> Self {
        Self::with_host(socket, EaglerHost::new(brand, version), None)
    }

    /// A shared world's end for a 1.8 guest: the plugin handshake offers
    /// protocol 47, the login is 1.8's, and the world's play packets are
    /// translated through `resolve` (a modern block state as 1.8's).
    pub fn legacy(
        socket: Box<dyn Messages>,
        brand: &str,
        version: &str,
        resolve: Arc<dyn Fn(u32) -> LegacyState + Send + Sync>,
    ) -> Self {
        Self::with_host(
            socket,
            EaglerHost::speaking(brand, version, PROTOCOL_1_8),
            Some(LegacyGuest {
                play: PlayTranslator::default(),
                guest: GuestTranslator,
                resolve,
            }),
        )
    }

    fn with_host(socket: Box<dyn Messages>, host: EaglerHost, legacy: Option<LegacyGuest>) -> Self {
        Self {
            socket,
            shared: Arc::new(Mutex::new(ServerShared {
                host,
                phase: ServerPhase::Hosting,
                incoming: Vec::new(),
                threshold: None,
                legacy,
                guest_config: false,
            })),
            outgoing: Vec::new(),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: self.socket.try_clone()?,
            shared: self.shared.clone(),
            outgoing: Vec::new(),
        })
    }

    /// The level's shape, for translating its chunks to a legacy guest.
    pub fn set_chunk_shape(&self, shape: crate::translate::legacy::play::ChunkShape) {
        if let Some(legacy) = &mut self.shared().legacy {
            legacy.play.set_chunk_shape(shape);
        }
    }

    /// The world's biomes by network id, for translating chunks to a legacy
    /// guest.
    pub fn set_biomes(&self, biomes: Arc<dyn Fn(u32) -> u8 + Send + Sync>) {
        if let Some(legacy) = &mut self.shared().legacy {
            legacy.play.set_biomes(biomes);
        }
    }

    /// The world's entity types by network id, for its 1.8 spawn packets.
    pub fn set_entities(&self, entities: Arc<dyn Fn(u32) -> Option<String> + Send + Sync>) {
        if let Some(legacy) = &mut self.shared().legacy {
            legacy.play.set_entities(entities);
        }
    }

    /// The world's item types by network id, for its 1.8 stacks.
    pub fn set_items(&self, items: Arc<dyn Fn(u32) -> Option<String> + Send + Sync>) {
        if let Some(legacy) = &mut self.shared().legacy {
            legacy.play.set_items(items);
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<horizon_os::time::Duration>) {
        self.socket.set_read_timeout(timeout);
    }

    pub fn shutdown(&self) {
        self.socket.shutdown();
    }

    fn shared(&self) -> MutexGuard<'_, ServerShared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The guest's next binary message.
    fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            match self.socket.recv()? {
                Some(Message::Binary(data)) if !data.is_empty() => return Ok(data),
                Some(_) => {}
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
    }

    fn take_incoming(&self, out: &mut [u8]) -> Option<usize> {
        let mut shared = self.shared();
        if shared.incoming.is_empty() {
            return None;
        }
        let n = shared.incoming.len().min(out.len());
        out[..n].copy_from_slice(&shared.incoming[..n]);
        shared.incoming.drain(..n);
        Some(n)
    }

    /// One message of the plugin handshake; when it finishes, the guest is
    /// presented to the world's login as a vanilla client.
    fn handshake_step(&mut self) -> io::Result<()> {
        let data = self.recv()?;
        let (replies, playing) = {
            let mut shared = self.shared();
            let action = shared
                .host
                .message(&data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let HostAction::Handshake(replies) = action else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a guest sent a game packet before logging in",
                ));
            };
            (replies, shared.host.is_playing())
        };
        for reply in replies {
            self.socket.send(Message::Binary(reply))?;
        }
        if playing {
            let (name, uuid) = {
                let shared = self.shared();
                let (name, uuid) = shared.host.player().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "the guest has no player")
                })?;
                (name.to_owned(), uuid)
            };
            // The backend always speaks the world's protocol, whatever the
            // guest was given.
            let intention = handshake::Serverbound::Intention(handshake::Intention {
                protocol: PROTOCOL_VERSION,
                host: "eaglercraft".into(),
                port: 0,
                intent: handshake::LOGIN,
            });
            let hello = login::Serverbound::Hello(login::ServerHello { name, uuid });
            let mut frames = Vec::new();
            for body in [intention.encode(), hello.encode()] {
                frames.extend_from_slice(&frame::encode(&body, None).map_err(io::Error::other)?);
            }
            let mut shared = self.shared();
            shared.incoming.extend_from_slice(&frames);
            shared.phase = ServerPhase::Backend;
        }
        Ok(())
    }

    /// Whether `data` belongs to the guest's configuration state: dropped
    /// until the guest acknowledges the forwarded finish.
    fn still_configuring(&self, data: &[u8]) -> bool {
        let mut shared = self.shared();
        if !shared.guest_config {
            return false;
        }
        if data == [0x03] {
            shared.guest_config = false;
        }
        true
    }

    /// One game message of a logged-in guest, framed for the world.
    fn game_step(&self) -> io::Result<()> {
        let data = self.recv()?;
        // Skins and voice are the plugin's, not the game's.
        if data.first() == Some(&0xEE) || self.still_configuring(&data) {
            return Ok(());
        }
        let threshold = self.shared().threshold;
        let framed = frame::encode(&data, threshold).map_err(io::Error::other)?;
        self.shared().incoming.extend_from_slice(&framed);
        Ok(())
    }

    /// One 26.2 play message: the three packet-id differences rewritten,
    /// then framed for the world.
    fn modern_guest_step(&self) -> io::Result<()> {
        let data = self.recv()?;
        if data.first() == Some(&0xEE) || self.still_configuring(&data) {
            return Ok(());
        }
        let threshold = self.shared().threshold;
        if let Some(body) = rewrite_serverbound_play(&data) {
            let framed = frame::encode(&body, threshold).map_err(io::Error::other)?;
            self.shared().incoming.extend_from_slice(&framed);
        }
        Ok(())
    }

    /// One 1.8 play message of a legacy guest, translated and framed for the
    /// world.
    fn guest_step(&self) -> io::Result<()> {
        let data = self.recv()?;
        if data.first() == Some(&0xEE) {
            return Ok(());
        }
        let (translated, threshold) = {
            let mut shared = self.shared();
            let threshold = shared.threshold;
            let translated = shared
                .legacy
                .as_mut()
                .and_then(|legacy| legacy.guest.from_guest(&data));
            (translated, threshold)
        };
        if let Some(body) = translated {
            let framed = frame::encode(&body, threshold).map_err(io::Error::other)?;
            self.shared().incoming.extend_from_slice(&framed);
        }
        Ok(())
    }
}

impl Read for EaglerServerSocket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(n) = self.take_incoming(out) {
                return Ok(n);
            }
            // The phase out of the lock: the steps take it again.
            let (phase, legacy, modern) = {
                let shared = self.shared();
                (
                    shared.phase,
                    shared.legacy.is_some(),
                    shared.host.protocol() == PROTOCOL_26_2 as u16,
                )
            };
            match phase {
                ServerPhase::Hosting => self.handshake_step()?,
                ServerPhase::Play if legacy => self.guest_step()?,
                ServerPhase::Play if modern => self.modern_guest_step()?,
                ServerPhase::Play => self.game_step()?,
                // The guest is already logged in: what it sends during the
                // backend's configuration (its login and configuration
                // acknowledgements) is not wanted yet.
                ServerPhase::Configuration => {
                    self.recv()?;
                }
                ServerPhase::Backend => return Err(io::ErrorKind::WouldBlock.into()),
                ServerPhase::Closed => return Ok(0),
            }
        }
    }
}

impl Write for EaglerServerSocket {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.outgoing.extend_from_slice(data);
        while let Some(contents) = frame::split(&mut self.outgoing).map_err(io::Error::other)? {
            let body = {
                let threshold = self.shared().threshold;
                frame::decompress(contents, threshold).map_err(io::Error::other)?
            };
            let mut message = None;
            let mut closed = false;
            {
                let mut shared = self.shared();
                match shared.phase {
                    ServerPhase::Backend => {
                        let mut r = Reader::new(&body);
                        let id = r.varint().map_err(io::Error::other)?;
                        match id {
                            crate::ids::login::clientbound::LOGIN_COMPRESSION => {
                                if let Ok(login::Clientbound::Compression(c)) =
                                    login::Clientbound::decode(id, r.rest())
                                {
                                    shared.threshold =
                                        (c.threshold >= 0).then_some(c.threshold as usize);
                                }
                            }
                            crate::ids::login::clientbound::LOGIN_FINISHED => {
                                let legacy = shared.legacy.is_some();
                                if legacy {
                                    // A 1.8 guest's login ends here: the plugin
                                    // sends it the profile the backend gave.
                                    if let Ok(login::Clientbound::LoginFinished(finished)) =
                                        login::Clientbound::decode(id, r.rest())
                                    {
                                        let reply = legacy_packets::login_success(
                                            &crate::crypto::uuid_string(finished.profile.uuid),
                                            &finished.profile.name,
                                        );
                                        self.socket.send(Message::Binary(reply))?;
                                    }
                                } else {
                                    // A modern guest runs the ordinary login:
                                    // give it the backend's finished profile
                                    // (compression stays off for the guest),
                                    // then relay the backend's configuration.
                                    self.socket.send(Message::Binary(body.clone()))?;
                                    shared.guest_config = true;
                                }
                                // The plugin acknowledged the backend's login
                                // itself: the guest neither sends nor sees it.
                                let ack =
                                    login::Serverbound::LoginAcknowledged(crate::packets::Empty)
                                        .encode();
                                let framed = frame::encode(&ack, shared.threshold)
                                    .map_err(io::Error::other)?;
                                shared.incoming.extend_from_slice(&framed);
                                shared.phase = ServerPhase::Configuration;
                            }
                            crate::ids::login::clientbound::LOGIN_DISCONNECT => {
                                shared.phase = ServerPhase::Closed;
                                closed = true;
                            }
                            _ => {}
                        }
                    }
                    ServerPhase::Configuration => {
                        let mut r = Reader::new(&body);
                        let id = r.varint().map_err(io::Error::other)?;
                        let modern = shared.host.protocol() == PROTOCOL_26_2 as u16;
                        let reply = match id {
                            crate::ids::configuration::clientbound::FINISH_CONFIGURATION => Some(
                                crate::packets::configuration::Serverbound::FinishConfiguration(
                                    crate::packets::Empty,
                                )
                                .encode(),
                            ),
                            crate::ids::configuration::clientbound::KEEP_ALIVE => {
                                match crate::packets::configuration::Clientbound::decode(
                                    id,
                                    r.rest(),
                                ) {
                                    Ok(crate::packets::configuration::Clientbound::KeepAlive(
                                        keep,
                                    )) => Some(
                                        crate::packets::configuration::Serverbound::KeepAlive(keep)
                                            .encode(),
                                    ),
                                    _ => None,
                                }
                            }
                            crate::ids::configuration::clientbound::PING => {
                                match crate::packets::configuration::Clientbound::decode(
                                    id,
                                    r.rest(),
                                ) {
                                    Ok(crate::packets::configuration::Clientbound::Ping(ping)) => {
                                        Some(
                                            crate::packets::configuration::Serverbound::Pong(ping)
                                                .encode(),
                                        )
                                    }
                                    _ => None,
                                }
                            }
                            crate::ids::configuration::clientbound::DISCONNECT => {
                                shared.phase = ServerPhase::Closed;
                                closed = true;
                                None
                            }
                            _ => None,
                        };
                        if let Some(reply) = reply {
                            let framed = frame::encode(&reply, shared.threshold)
                                .map_err(io::Error::other)?;
                            shared.incoming.extend_from_slice(&framed);
                        }
                        // A modern guest runs the backend's configuration;
                        // 1.8 has none. Keep-alives are answered above, and
                        // the guest's own answers are dropped by `Read`.
                        if shared.legacy.is_none()
                            && !matches!(
                                id,
                                crate::ids::configuration::clientbound::KEEP_ALIVE
                                    | crate::ids::configuration::clientbound::PING
                                    | crate::ids::configuration::clientbound::DISCONNECT
                            )
                        {
                            message = if modern {
                                rewrite_clientbound_configuration(&body).map(Message::Binary)
                            } else {
                                Some(Message::Binary(body.clone()))
                            };
                        }
                        if id == crate::ids::configuration::clientbound::FINISH_CONFIGURATION {
                            shared.phase = ServerPhase::Play;
                        }
                    }
                    ServerPhase::Play => {
                        message = if let Some(legacy) = shared.legacy.as_mut() {
                            legacy
                                .play
                                .translate(&body, &*legacy.resolve)
                                .map(Message::Binary)
                        } else if shared.host.protocol() == PROTOCOL_26_2 as u16 {
                            rewrite_clientbound_play(&body).map(Message::Binary)
                        } else {
                            Some(Message::Binary(body))
                        };
                    }
                    _ => {}
                }
            }
            if closed {
                self.socket.shutdown();
                return Ok(data.len());
            }
            if let Some(message) = message {
                self.socket.send(message)?;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connection::Connection,
        eagler::{Message, Messages},
        packets::{handshake, login},
    };
    use std::{
        collections::VecDeque,
        io::{self, Read, Write},
        sync::{Arc, Mutex},
    };

    /// The host behind an in-memory socket: every message the client sends is
    /// fed to `EaglerHost`, and its replies go back to the client.
    struct Hosting(Arc<Host>);
    struct Host {
        host: Mutex<EaglerHost>,
        to_client: Mutex<VecDeque<Message>>,
        sent: Mutex<Vec<Message>>,
    }
    impl Messages for Hosting {
        fn send(&self, message: Message) -> io::Result<()> {
            self.0.sent.lock().unwrap().push(message.clone());
            if let Message::Binary(data) = message {
                let mut host = self.0.host.lock().unwrap();
                if let Ok(HostAction::Handshake(replies)) = host.message(&data) {
                    let mut to_client = self.0.to_client.lock().unwrap();
                    for reply in replies {
                        to_client.push_back(Message::Binary(reply));
                    }
                    if host.is_playing() {
                        // A game packet the host sends after the login.
                        to_client.push_back(Message::Binary(vec![0x03, 7]));
                    }
                }
            }
            Ok(())
        }
        fn recv(&self) -> io::Result<Option<Message>> {
            Ok(self.0.to_client.lock().unwrap().pop_front())
        }
        fn set_read_timeout(&self, _: Option<horizon_os::time::Duration>) {}
        fn shutdown(&self) {}
        fn try_clone(&self) -> io::Result<Box<dyn Messages>> {
            Ok(Box::new(Hosting(self.0.clone())))
        }
    }

    /// The guest's side of the transport: messages it sent, and those the
    /// world sent it.
    struct GuestSocket(Arc<GuestState>);
    struct GuestState {
        incoming: Mutex<VecDeque<Message>>,
        sent: Mutex<Vec<Message>>,
    }
    impl Messages for GuestSocket {
        fn send(&self, message: Message) -> io::Result<()> {
            self.0.sent.lock().unwrap().push(message);
            Ok(())
        }
        fn recv(&self) -> io::Result<Option<Message>> {
            Ok(self.0.incoming.lock().unwrap().pop_front())
        }
        fn set_read_timeout(&self, _: Option<horizon_os::time::Duration>) {}
        fn shutdown(&self) {}
        fn try_clone(&self) -> io::Result<Box<dyn Messages>> {
            Ok(Box::new(GuestSocket(self.0.clone())))
        }
    }

    fn client_version(name: &str, minecraft: &[u16]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x01);
        w.u8(0x02);
        w.u16(1);
        w.u16(EAGLER_PROTOCOL);
        w.u16(minecraft.len() as u16);
        for &protocol in minecraft {
            w.u16(protocol);
        }
        str8_write(&mut w, "Horizon");
        str8_write(&mut w, "0.1.0");
        str8_write(&mut w, "");
        str8_write(&mut w, name);
        w.data
    }

    /// The 26.2 client's CLIENT_VERSION: the same lists, then a false auth
    /// flag before the name.
    fn client_version_26_2(name: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x01);
        w.u8(0x02);
        w.u16(3);
        for protocol in [3u16, 4, 5] {
            w.u16(protocol);
        }
        w.u16(1);
        w.u16(776);
        str8_write(&mut w, "Eaglercraft 26.2");
        str8_write(&mut w, "26.2");
        str8_write(&mut w, "");
        str8_write(&mut w, name);
        w.data
    }

    fn request_login(name: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x04);
        str8_write(&mut w, name);
        str8_write(&mut w, "default");
        w.data.extend_from_slice(&[0, 0, 0, 0, 0]);
        w.data
    }

    fn handshake(action: &HostAction) -> &[Vec<u8>] {
        match action {
            HostAction::Handshake(messages) => messages,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn logs_a_guest_in_the_way_the_browser_client_expects() {
        let protocol = u16::try_from(PROTOCOL_VERSION).unwrap();
        let mut host = EaglerHost::new("Horizon", "0.1.0");
        let version = client_version("Steve", &[protocol]);
        let reply = host.message(&version).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0][0], 0x02);
        assert_eq!(&reply[0][1..3], &EAGLER_PROTOCOL.to_be_bytes());
        assert_eq!(&reply[0][3..5], &protocol.to_be_bytes());
        assert_eq!(host.client(), ("Horizon", "0.1.0"));

        let reply = host.message(&request_login("Steve")).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply[0][0], 0x05);
        assert_eq!(reply[0][1] as usize, "Steve".len());
        assert_eq!(&reply[0][2..2 + "Steve".len()], b"Steve");
        let uuid = crate::crypto::offline_uuid("Steve");
        let at = 2 + "Steve".len();
        assert_eq!(&reply[0][at..at + 16], &uuid.to_be_bytes());
        assert_eq!(&reply[0][at + 16..], &[0, 0]);

        // A skin, then FINISH_LOGIN.
        let mut profile = vec![0x07, 1, 7];
        profile.extend_from_slice(b"skin_v2");
        profile.extend_from_slice(&3u16.to_be_bytes());
        profile.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            handshake(&host.message(&profile).unwrap()),
            &[] as &[Vec<u8>]
        );
        assert_eq!(
            host.profile(),
            [("skin_v2".to_owned(), vec![1, 2, 3])].as_slice()
        );

        let reply = host.message(&[0x08]).unwrap();
        assert_eq!(handshake(&reply), &[vec![0x09]][..]);
        assert!(host.is_playing());
        assert_eq!(host.player(), Some(("Steve", uuid)));

        // Play: game packets pass through, the plugin channel without 0xEE.
        assert_eq!(
            host.message(&[0x03, 7]).unwrap(),
            HostAction::Play(vec![0x03, 7])
        );
        assert_eq!(
            host.message(&[0xEE, 1, 2]).unwrap(),
            HostAction::Plugin(vec![1, 2])
        );
    }

    #[test]
    fn refuses_a_guest_that_speaks_another_protocol() {
        let mut host = EaglerHost::new("Horizon", "0.1.0");
        let reply = host.message(&client_version("Old", &[47])).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply[0][0], 0x03);
        assert!(!host.is_playing());
        assert!(host.message(&request_login("Old")).is_err());
    }

    #[test]
    fn accepts_a_26_2_client_and_asks_for_its_name() {
        let mut host = EaglerHost::new("Horizon", "0.1.0");
        let reply = host.message(&client_version_26_2("Steve")).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply[0][0], 0x02);
        assert_eq!(&reply[0][1..3], &EAGLER_PROTOCOL.to_be_bytes());
        assert_eq!(&reply[0][3..5], &776u16.to_be_bytes());
        assert_eq!(*reply[0].last().unwrap(), 1, "nickname selection");
        assert_eq!(host.protocol(), 776);

        // The 26.2 login request has the same leading fields and more zero
        // capability bytes, which the host ignores.
        let reply = host.message(&request_login("Steve")).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply[0][0], 0x05);
        let uuid = crate::crypto::offline_uuid("Steve");
        let at = 2 + "Steve".len();
        assert_eq!(&reply[0][2..at], b"Steve");
        assert_eq!(&reply[0][at..at + 16], &uuid.to_be_bytes());

        let mut profile = vec![0x07, 2];
        for (kind, data) in [("skin_v2", &[1, 2, 3][..]), ("cape_v1", &[4, 5][..])] {
            profile.push(kind.len() as u8);
            profile.extend_from_slice(kind.as_bytes());
            profile.extend_from_slice(&(data.len() as u16).to_be_bytes());
            profile.extend_from_slice(data);
        }
        host.message(&profile).unwrap();
        let reply = host.message(&[0x08]).unwrap();
        assert_eq!(handshake(&reply), &[vec![0x09]][..]);
        assert!(host.is_playing());
        assert_eq!(host.player(), Some(("Steve", uuid)));
    }

    #[test]
    fn refuses_a_login_without_a_name() {
        let protocol = u16::try_from(PROTOCOL_VERSION).unwrap();
        let mut host = EaglerHost::new("Horizon", "0.1.0");
        host.message(&client_version("", &[protocol])).unwrap();
        let reply = host.message(&request_login("")).unwrap();
        let reply = handshake(&reply);
        assert_eq!(reply[0][0], 0x06);
        let len = usize::from(u16::from_be_bytes([reply[0][1], reply[0][2]]));
        assert_eq!(
            &reply[0][3..3 + len],
            br#"{"text":"A name is required to join."}"#
        );
    }

    #[test]
    fn the_client_version_offers_its_protocols() {
        assert_eq!(
            client_protocols(&client_version("Steve", &[PROTOCOL_1_8])),
            Some(vec![PROTOCOL_1_8])
        );
        assert_eq!(
            client_protocols(&client_version(
                "Steve",
                &[PROTOCOL_1_8, u16::try_from(PROTOCOL_VERSION).unwrap()]
            )),
            Some(vec![PROTOCOL_1_8, u16::try_from(PROTOCOL_VERSION).unwrap()])
        );
        assert_eq!(client_protocols(&[0x08]), None);
    }

    #[test]
    fn handshake_messages_have_to_come_in_order() {
        let mut host = EaglerHost::new("Horizon", "0.1.0");
        assert!(host.message(&[0x03, 7]).is_err());
        assert!(host.message(&[]).is_err());
    }

    /// The browser client's own handshake against this host, over an
    /// in-memory socket: the login finishes with the name and UUID the host
    /// gave, then game packets flow both ways.
    #[test]
    fn the_browser_client_logs_in_to_this_host() {
        let hosting = Arc::new(Host {
            host: Mutex::new(EaglerHost::new("Horizon", "0.1.0")),
            to_client: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        });
        let mut client = Connection::eagler(Box::new(Hosting(hosting.clone()))).unwrap();
        client
            .send(
                &handshake::Serverbound::Intention(handshake::Intention {
                    protocol: 777,
                    host: "lan".into(),
                    port: 0,
                    intent: handshake::LOGIN,
                })
                .encode(),
            )
            .unwrap();
        let uuid = crate::crypto::offline_uuid("Steve");
        client
            .send(
                &login::Serverbound::Hello(login::ServerHello {
                    name: "Steve".into(),
                    uuid,
                })
                .encode(),
            )
            .unwrap();
        let (id, payload) = client.recv().unwrap();
        match login::Clientbound::decode(id, &payload).unwrap() {
            login::Clientbound::LoginFinished(profile) => {
                assert_eq!(profile.profile.name, "Steve");
                assert_eq!(profile.profile.uuid, uuid);
            }
            other => panic!("{other:?}"),
        }
        client
            .send(&login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode())
            .unwrap();
        assert_eq!(client.recv().unwrap(), (3, vec![7]));
        assert_eq!(hosting.host.lock().unwrap().player(), Some(("Steve", uuid)));
        let sent = hosting.sent.lock().unwrap();
        let ids: Vec<u8> = sent
            .iter()
            .filter_map(|message| match message {
                Message::Binary(data) => data.first().copied(),
                Message::Text(_) => None,
            })
            .collect();
        assert_eq!(ids.first(), Some(&0x01), "CLIENT_VERSION first");
        assert_eq!(ids.last(), Some(&0x08), "FINISH_LOGIN last");
    }

    /// The world's end of the transport: the guest's plugin handshake becomes
    /// the vanilla handshake and hello the backend reads; the backend's
    /// compression and login-finished packets are handled as the plugin does;
    /// then game packets travel uncompressed both ways.
    #[test]
    fn the_transport_presents_a_guest_to_the_world() {
        let protocol = u16::try_from(PROTOCOL_VERSION).unwrap();
        let guest = Arc::new(GuestState {
            incoming: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        });
        guest
            .incoming
            .lock()
            .unwrap()
            .extend([Message::Binary(client_version("Steve", &[protocol]))]);
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(request_login("Steve")));
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0x08]));
        let mut server =
            EaglerServerSocket::new(Box::new(GuestSocket(guest.clone())), "Horizon", "0.1.0");
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();
        // One frame, reading more only when nothing is buffered: a read may
        // return several.
        let mut next_frame = |server: &mut EaglerServerSocket, pending: &mut Vec<u8>| -> Vec<u8> {
            loop {
                if let Some(frame) = frame::split(pending).unwrap() {
                    return frame;
                }
                let n = server.read(&mut buf).unwrap();
                assert!(n > 0, "the transport closed early");
                pending.extend_from_slice(&buf[..n]);
            }
        };

        // The guest's plugin login, as the world sees it: a handshake...
        let body = next_frame(&mut server, &mut pending);
        assert_eq!(
            body,
            handshake::Serverbound::Intention(handshake::Intention {
                protocol: i32::from(protocol),
                host: "eaglercraft".into(),
                port: 0,
                intent: handshake::LOGIN,
            })
            .encode()
        );
        // ...and the hello the plugin makes for it.
        let body = next_frame(&mut server, &mut pending);
        let mut r = Reader::new(&body);
        let id = r.varint().unwrap();
        match login::Serverbound::decode(id, r.rest()).unwrap() {
            login::Serverbound::Hello(hello) => {
                assert_eq!(hello.name, "Steve");
                assert_eq!(hello.uuid, crate::crypto::offline_uuid("Steve"));
            }
            other => panic!("{other:?}"),
        }
        {
            let sent = guest.sent.lock().unwrap();
            assert_eq!(sent.len(), 3);
            assert_eq!(sent[0], Message::Binary(server_version_bytes(protocol)));
        }

        // The backend's login: compression (swallowed), then login finished
        // (forwarded to the guest; its acknowledgement is supplied to the
        // backend).
        let compression =
            login::Clientbound::Compression(login::Compression { threshold: 64 }).encode();
        server
            .write_all(&frame::encode(&compression, None).unwrap())
            .unwrap();
        let uuid = crate::crypto::offline_uuid("Steve");
        let finished = login::Clientbound::LoginFinished(login::LoginFinished {
            profile: login::GameProfile {
                uuid,
                name: "Steve".into(),
                properties: Vec::new(),
            },
            session: uuid,
        })
        .encode();
        server
            .write_all(&frame::encode(&finished, Some(64)).unwrap())
            .unwrap();
        let contents = next_frame(&mut server, &mut pending);
        let body = frame::decompress(contents, Some(64)).unwrap();
        assert_eq!(
            body,
            login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode()
        );
        {
            let sent = guest.sent.lock().unwrap();
            assert_eq!(sent.len(), 4);
            assert_eq!(sent[3], Message::Binary(finished.clone()));
        }

        // Configuration: the backend finishes; the guest is given the same
        // packet, and its acknowledgement is supplied to the backend.
        let finish =
            crate::packets::configuration::Clientbound::FinishConfiguration(crate::packets::Empty)
                .encode();
        server
            .write_all(&frame::encode(&finish, Some(64)).unwrap())
            .unwrap();
        let contents = next_frame(&mut server, &mut pending);
        assert_eq!(
            frame::decompress(contents, Some(64)).unwrap(),
            crate::packets::configuration::Serverbound::FinishConfiguration(crate::packets::Empty)
                .encode()
        );
        assert_eq!(
            guest.sent.lock().unwrap().last(),
            Some(&Message::Binary(finish))
        );
        // The guest's configuration acknowledgement, then its game packets.
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0x03]));

        // The world's game packets reach the guest uncompressed...
        let play = vec![0x07, 1, 2];
        server
            .write_all(&frame::encode(&play, Some(64)).unwrap())
            .unwrap();
        assert_eq!(
            guest.sent.lock().unwrap().last(),
            Some(&Message::Binary(play.clone()))
        );
        // ...and the guest's reach the world framed at its threshold; the
        // plugin channel is not a game packet.
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0xEE, 9]));
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0x05, 6]));
        let contents = next_frame(&mut server, &mut pending);
        assert_eq!(
            frame::decompress(contents, Some(64)).unwrap(),
            vec![0x05, 6]
        );
    }

    /// One frame from the transport, reading more only when nothing is
    /// buffered.
    fn next_frame(
        server: &mut EaglerServerSocket,
        pending: &mut Vec<u8>,
        buf: &mut [u8],
    ) -> Vec<u8> {
        loop {
            if let Some(frame) = frame::split(pending).unwrap() {
                return frame;
            }
            let n = server.read(buf).unwrap();
            assert!(n > 0, "the transport closed early");
            pending.extend_from_slice(&buf[..n]);
        }
    }

    /// A protocol-776 guest: ordinary modern login and configuration, with
    /// the three 26.3 packet-id differences rewritten both ways.
    #[test]
    fn a_26_2_guest_logs_in_and_has_its_packet_ids_rewritten() {
        let guest = Arc::new(GuestState {
            incoming: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        });
        for message in [
            client_version_26_2("Steve"),
            request_login("Steve"),
            vec![0x08],
        ] {
            guest
                .incoming
                .lock()
                .unwrap()
                .push_back(Message::Binary(message));
        }
        let mut server =
            EaglerServerSocket::new(Box::new(GuestSocket(guest.clone())), "Horizon", "0.1.0");
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();

        // The synthesized backend handshake and hello.
        let _ = next_frame(&mut server, &mut pending, &mut buf);
        let _ = next_frame(&mut server, &mut pending, &mut buf);

        // The backend's login, forwarded to the guest (compression stays off
        // for the guest).
        let compression =
            login::Clientbound::Compression(login::Compression { threshold: 64 }).encode();
        server
            .write_all(&frame::encode(&compression, None).unwrap())
            .unwrap();
        let uuid = crate::crypto::offline_uuid("Steve");
        let finished = login::Clientbound::LoginFinished(login::LoginFinished {
            profile: login::GameProfile {
                uuid,
                name: "Steve".into(),
                properties: Vec::new(),
            },
            session: uuid,
        })
        .encode();
        server
            .write_all(&frame::encode(&finished, Some(64)).unwrap())
            .unwrap();
        {
            let sent = guest.sent.lock().unwrap();
            assert_eq!(sent.len(), 4);
            assert_eq!(sent[3], Message::Binary(finished));
        }

        // The plugin's acknowledgement of the backend's login.
        let login_ack = next_frame(&mut server, &mut pending, &mut buf);
        assert_eq!(
            frame::decompress(login_ack, Some(64)).unwrap(),
            login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode()
        );

        // Configuration: the backend finishes, the guest gets the finish, and
        // the backend gets the acknowledgement.
        let finish =
            crate::packets::configuration::Clientbound::FinishConfiguration(crate::packets::Empty)
                .encode();
        server
            .write_all(&frame::encode(&finish, Some(64)).unwrap())
            .unwrap();
        let ack = next_frame(&mut server, &mut pending, &mut buf);
        assert_eq!(
            frame::decompress(ack, Some(64)).unwrap(),
            crate::packets::configuration::Serverbound::FinishConfiguration(crate::packets::Empty)
                .encode()
        );
        assert_eq!(
            guest.sent.lock().unwrap().last(),
            Some(&Message::Binary(finish))
        );

        // The guest acknowledges configuration, then both directions play.
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0x03]));
        let mut play = Writer::new();
        play.varint(38); // 26.3's id for the first shifted packet
        play.data.push(0xAA);
        server
            .write_all(&frame::encode(&play.data, Some(64)).unwrap())
            .unwrap();
        {
            let sent = guest.sent.lock().unwrap();
            let Some(Message::Binary(got)) = sent.last() else {
                panic!("{sent:?}")
            };
            assert_eq!(got, &vec![37, 0xAA]);
        }

        // 26.2's swing(hand) reaches the world as 26.3's empty punch.
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![63, 0]));
        let body = next_frame(&mut server, &mut pending, &mut buf);
        assert_eq!(frame::decompress(body, Some(64)).unwrap(), vec![46]);
    }

    /// A protocol-47 guest: its login ends at the 1.8 login success, the
    /// backend's configuration is discarded (1.8 has none), and play packets
    /// are translated both ways.
    #[test]
    fn a_1_8_guest_logs_in_and_moves() {
        use crate::packets::play::{Clientbound as Play, Login, Serverbound, SpawnInfo};
        let guest = Arc::new(GuestState {
            incoming: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        });
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(client_version("Steve", &[PROTOCOL_1_8])));
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(request_login("Steve")));
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(vec![0x08]));
        let mut server = EaglerServerSocket::legacy(
            Box::new(GuestSocket(guest.clone())),
            "Horizon",
            "0.1.0",
            Arc::new(|_| LegacyState { id: 1, data: 0 }),
        );
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();

        // The backend is spoken to in the world's protocol, whatever the guest
        // offered.
        let body = next_frame(&mut server, &mut pending, &mut buf);
        let mut r = Reader::new(&body);
        let id = r.varint().unwrap();
        match handshake::Serverbound::decode(id, r.rest()).unwrap() {
            handshake::Serverbound::Intention(intention) => {
                assert_eq!(intention.protocol, PROTOCOL_VERSION);
            }
            other => panic!("{other:?}"),
        }
        let body = next_frame(&mut server, &mut pending, &mut buf);
        let mut r = Reader::new(&body);
        let id = r.varint().unwrap();
        match login::Serverbound::decode(id, r.rest()).unwrap() {
            login::Serverbound::Hello(hello) => assert_eq!(hello.name, "Steve"),
            other => panic!("{other:?}"),
        }

        // The backend's login: compression, then its finished profile.
        let compression =
            login::Clientbound::Compression(login::Compression { threshold: 64 }).encode();
        server
            .write_all(&frame::encode(&compression, None).unwrap())
            .unwrap();
        let uuid = crate::crypto::offline_uuid("Steve");
        let finished = login::Clientbound::LoginFinished(login::LoginFinished {
            profile: login::GameProfile {
                uuid,
                name: "Steve".into(),
                properties: Vec::new(),
            },
            session: uuid,
        })
        .encode();
        server
            .write_all(&frame::encode(&finished, Some(64)).unwrap())
            .unwrap();
        let sent = guest.sent.lock().unwrap();
        assert_eq!(sent.len(), 4);
        let Message::Binary(success) = &sent[3] else {
            panic!("{sent:?}")
        };
        let mut r = Reader::new(success);
        assert_eq!(r.varint().unwrap(), 0x02);
        assert_eq!(
            legacy_packets::read_string(&mut r).unwrap(),
            crate::crypto::uuid_string(uuid)
        );
        assert_eq!(legacy_packets::read_string(&mut r).unwrap(), "Steve");
        drop(sent);

        // The plugin's acknowledgement of the backend's login.
        let login_ack = next_frame(&mut server, &mut pending, &mut buf);
        assert_eq!(
            frame::decompress(login_ack, Some(64)).unwrap(),
            login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode()
        );

        // Configuration: finished at once, and its acknowledgement supplied.
        let finish =
            crate::packets::configuration::Clientbound::FinishConfiguration(crate::packets::Empty)
                .encode();
        server
            .write_all(&frame::encode(&finish, Some(64)).unwrap())
            .unwrap();
        let ack = next_frame(&mut server, &mut pending, &mut buf);
        assert_eq!(
            frame::decompress(ack, Some(64)).unwrap(),
            crate::packets::configuration::Serverbound::FinishConfiguration(crate::packets::Empty)
                .encode()
        );

        // The world's join game reaches the guest as 1.8's.
        let join = Play::Login(Login {
            player_id: 7,
            hardcore: false,
            levels: vec!["minecraft:overworld".into()],
            max_players: 20,
            chunk_radius: 10,
            simulation_distance: 10,
            reduced_debug_info: false,
            show_death_screen: true,
            limited_crafting: false,
            spawn: SpawnInfo {
                dimension_type: 0,
                dimension: "minecraft:overworld".into(),
                seed: 0,
                game_type: 1,
                previous_game_type: None,
                debug: false,
                flat: false,
                last_death: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            online_mode: false,
            enforces_secure_chat: false,
        });
        server
            .write_all(&frame::encode(&join.encode(), Some(64)).unwrap())
            .unwrap();
        let sent = guest.sent.lock().unwrap();
        let Message::Binary(join) = sent.last().unwrap() else {
            panic!("{sent:?}")
        };
        let mut r = Reader::new(join);
        assert_eq!(r.varint().unwrap(), 0x01);
        assert_eq!(r.i32().unwrap(), 7);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 2);
        drop(sent);

        // The guest's movement reaches the world as a modern move.
        let mut movement = Writer::new();
        movement.varint(crate::translate::legacy::ids::play::serverbound::PLAYER_POSITION_AND_LOOK);
        movement.f64(1.5);
        movement.f64(64.0);
        movement.f64(-2.0);
        movement.f32(90.0);
        movement.f32(0.0);
        movement.bool(true);
        guest
            .incoming
            .lock()
            .unwrap()
            .push_back(Message::Binary(movement.data));
        let body = next_frame(&mut server, &mut pending, &mut buf);
        let modern = frame::decompress(body, Some(64)).unwrap();
        let mut r = Reader::new(&modern);
        let id = r.varint().unwrap();
        match Serverbound::decode(id, r.rest()).unwrap() {
            Serverbound::MovePlayerPosRot(m) => {
                assert_eq!((m.x, m.y, m.z), (1.5, 64.0, -2.0));
                assert_eq!((m.yaw, m.pitch), (90.0, 0.0));
                assert_eq!(m.flags, 1);
            }
            other => panic!("{other:?}"),
        }
    }

    fn server_version_bytes(protocol: u16) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(0x02);
        w.u16(EAGLER_PROTOCOL);
        w.u16(protocol);
        str8_write(&mut w, "Horizon");
        str8_write(&mut w, "0.1.0");
        w.data.extend_from_slice(&[0, 0, 0, 0]);
        w.data
    }
}
