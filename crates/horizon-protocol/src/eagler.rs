//! A client's end of an Eaglercraft server: the EaglerXServer plugin in front
//! of a Minecraft proxy, reached from the browser by a `ws:`/`wss:` URL.
//!
//! The plugin logs in to the backend for its player: the client offers Eagler
//! protocol 5 with this game's protocol number, then sends its name, and the
//! proxy makes the handshake, login start and login acknowledgement itself.
//! What follows is the plain game protocol, one packet per binary message
//! with no length prefix, never compressed or encrypted (so online-mode
//! backends are out). A server's status is a text query, `Accept: motd`.
//!
//! The client wears the skin and cape set with [`set_look`]: the login sends
//! them (`skin_v2`, `cape_v1` profile data), and the plugin shows them to
//! the other players. Theirs come from the plugin's messages
//! ([`other_look`]), which start with `0xEE` in the play phase.
//!
//! [`EaglerSocket`] hides all of that behind the byte stream a
//! [`Connection`](crate::connection::Connection) reads and writes: it reads
//! the client's handshake and login packets, answers them with what the
//! plugin said (as a vanilla server would), and from then on turns frames
//! into messages and back.
use crate::{
    buf::{Reader, Writer},
    frame,
    ids::login::serverbound::LOGIN_ACKNOWLEDGED,
    packets::{handshake, login, status},
};
use horizon_os::time::Duration;
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};

/// One WebSocket message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Binary(Vec<u8>),
    Text(String),
}

/// A message socket: the browser's WebSocket on the web, a fake in tests.
pub trait Messages: Send + Sync {
    fn send(&self, message: Message) -> io::Result<()>;
    /// The next message, waiting up to the read timeout; `None` once closed.
    fn recv(&self) -> io::Result<Option<Message>>;
    fn set_read_timeout(&self, timeout: Option<Duration>);
    fn shutdown(&self);
    /// Another handle on the same socket, with its own read timeout.
    fn try_clone(&self) -> io::Result<Box<dyn Messages>>;
}

/// How long each step of the plugin's login may take: it gives a client
/// 10 s for the whole handshake, and the backend's login comes on top.
const STEP: Duration = Duration::from_secs(30);

/// The Eagler protocol offered: 5 is the one whose Minecraft protocol list
/// isn't capped at 1.12's (340) by default.
const EAGLER_PROTOCOL: u16 = 5;

/// What the client has said so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Waiting for the handshake's intention.
    Intention,
    Status,
    /// Waiting for the login start, which runs the plugin's login.
    Login,
    /// The plugin acknowledged the login itself: the client's own
    /// acknowledgement is dropped.
    Acknowledge,
    /// Frames are messages.
    Play,
    /// The login failed; the reason is the last thing to read.
    Closed,
}

struct Shared {
    phase: Phase,
    /// The game protocol from the client's intention.
    protocol: i32,
    /// Frames to read, answers made here and messages turned back into frames.
    incoming: Vec<u8>,
}

pub struct EaglerSocket {
    socket: Box<dyn Messages>,
    shared: Arc<Mutex<Shared>>,
    /// Bytes written short of a whole frame.
    outgoing: Vec<u8>,
}

impl EaglerSocket {
    pub fn new(socket: Box<dyn Messages>) -> Self {
        Self {
            socket,
            shared: Arc::new(Mutex::new(Shared {
                phase: Phase::Intention,
                protocol: crate::PROTOCOL_VERSION,
                incoming: Vec::new(),
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

    pub fn set_read_timeout(&self, timeout: Option<Duration>) {
        self.socket.set_read_timeout(timeout);
    }

    pub fn shutdown(&self) {
        self.socket.shutdown();
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a packet body for the client to read, as a frame.
    fn answer(&self, body: &[u8]) {
        let framed = frame::prefixed(body);
        self.shared().incoming.extend_from_slice(&framed);
    }

    /// One frame the client wrote (never compressed: the plugin keeps
    /// compression off).
    fn handle(&mut self, body: Vec<u8>) -> io::Result<()> {
        let phase = self.shared().phase;
        if phase == Phase::Play {
            let body = if self.shared().protocol == crate::translate::modern::PROTOCOL_26_2 {
                match crate::translate::modern::rewrite_serverbound_from_26_3(&body) {
                    Some(body) => body,
                    None => return Ok(()),
                }
            } else {
                body
            };
            return self.socket.send(Message::Binary(body));
        }
        let mut r = Reader::new(&body);
        let id = r.varint().map_err(io::Error::other)?;
        let payload = r.rest();
        match phase {
            Phase::Intention => {
                if let Ok(handshake::Serverbound::Intention(i)) =
                    handshake::Serverbound::decode(id, payload)
                {
                    let mut shared = self.shared();
                    shared.protocol = i.protocol;
                    shared.phase = if i.intent == handshake::STATUS {
                        Phase::Status
                    } else {
                        Phase::Login
                    };
                }
            }
            Phase::Status => match status::Serverbound::decode(id, payload) {
                Ok(status::Serverbound::StatusRequest(_)) => {
                    let json = self.query_status()?;
                    self.answer(
                        &status::Clientbound::StatusResponse(status::StatusResponse { json })
                            .encode(),
                    );
                }
                Ok(status::Serverbound::PingRequest(ping)) => {
                    self.answer(&status::Clientbound::PongResponse(ping).encode());
                }
                _ => {}
            },
            Phase::Login => {
                if let Ok(login::Serverbound::Hello(hello)) =
                    login::Serverbound::decode(id, payload)
                {
                    self.socket.set_read_timeout(Some(STEP));
                    let result = self.log_in(&hello.name);
                    self.socket.set_read_timeout(None);
                    match result {
                        Ok((name, uuid)) => {
                            self.answer(
                                &login::Clientbound::LoginFinished(login::LoginFinished {
                                    profile: login::GameProfile {
                                        uuid,
                                        name,
                                        properties: Vec::new(),
                                    },
                                    session: uuid,
                                })
                                .encode(),
                            );
                            self.shared().phase = Phase::Acknowledge;
                        }
                        Err(reason_json) => {
                            self.answer(
                                &login::Clientbound::Disconnect(login::Disconnect { reason_json })
                                    .encode(),
                            );
                            self.shared().phase = Phase::Closed;
                        }
                    }
                }
            }
            Phase::Acknowledge => {
                if id == LOGIN_ACKNOWLEDGED {
                    self.shared().phase = Phase::Play;
                }
            }
            Phase::Play | Phase::Closed => {}
        }
        Ok(())
    }

    /// The next binary message during the login, `Err` with a reason the
    /// client shows.
    fn next(&self) -> Result<Vec<u8>, String> {
        loop {
            match self.socket.recv() {
                Ok(Some(Message::Binary(data))) if !data.is_empty() => return Ok(data),
                Ok(Some(_)) => {}
                Ok(None) => return Err(text("Connection closed by the server")),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(text("Timed out"));
                }
                Err(e) => return Err(text(&e.to_string())),
            }
        }
    }

    fn send(&self, data: Vec<u8>) -> Result<(), String> {
        self.socket
            .send(Message::Binary(data))
            .map_err(|e| text(&e.to_string()))
    }

    /// The plugin's login (EaglerXServer's HandshakerV5): the name and UUID
    /// the server gave, or the reason it refused, as a JSON component.
    fn log_in(&self, name: &str) -> Result<(String, u128), String> {
        let protocol = self.shared().protocol;
        // CLIENT_VERSION: the "legacy" marker 2, the Eagler protocols, the
        // Minecraft ones (this world's 777 first, then 26.2's 776 so a 26.2
        // host can take us), brand, version, no password, the name.
        let mut w = Writer::new();
        w.u8(0x01);
        w.u8(0x02);
        w.u16(1);
        w.u16(EAGLER_PROTOCOL);
        w.u16(2);
        w.u16(u16::try_from(protocol).unwrap_or(u16::MAX));
        w.u16(u16::try_from(crate::translate::modern::PROTOCOL_26_2).unwrap_or(u16::MAX));
        str8(&mut w, "Horizon");
        str8(&mut w, env!("CARGO_PKG_VERSION"));
        w.u8(0);
        str8(&mut w, name);
        self.send(w.data)?;
        let reply = self.next()?;
        refusal(&reply)?;
        if reply[0] != 0x02 {
            return Err(text("Unexpected reply from the Eaglercraft server"));
        }
        // SERVER_VERSION carries the Eagler and Minecraft protocols the host
        // chose; accept only this world's or 26.2's, and remember which.
        let selected = (|| {
            let mut r = Reader::new(&reply);
            r.u8().ok()?;
            r.u16().ok()?;
            r.u16().ok()
        })()
        .ok_or_else(|| text("Unexpected reply from the Eaglercraft server"))?;
        if selected != u16::try_from(protocol).unwrap_or(u16::MAX)
            && selected
                != u16::try_from(crate::translate::modern::PROTOCOL_26_2).unwrap_or(u16::MAX)
        {
            return Err(text(
                "The Eaglercraft server chose an unsupported Minecraft protocol",
            ));
        }
        self.shared().protocol = i32::from(selected);
        // REQUEST_LOGIN: the name, the default server, no password, no
        // cookie, no capabilities.
        let mut w = Writer::new();
        w.u8(0x04);
        str8(&mut w, name);
        str8(&mut w, "default");
        w.u8(0);
        w.u8(0);
        w.u8(0);
        w.varint(0);
        w.u8(0);
        self.send(w.data)?;
        // ALLOW_LOGIN comes once the backend took the login.
        let reply = self.next()?;
        refusal(&reply)?;
        let allowed = (|| {
            let mut r = Reader::new(&reply);
            if r.u8()? != 0x05 {
                return Err(crate::Error::new("not ALLOW_LOGIN"));
            }
            let n = r.u8()?;
            let name = String::from_utf8_lossy(r.bytes(n.into())?).into_owned();
            let uuid = (u128::from(r.i64()? as u64) << 64) | u128::from(r.i64()? as u64);
            Ok((name, uuid))
        })()
        .map_err(|_| text("Unexpected reply from the Eaglercraft server"))?;
        // PROFILE_DATA with the skin and cape, if set (without, the plugin
        // gives the default ones), then FINISH_LOGIN.
        if let Some(data) = profile_data() {
            self.send(data)?;
        }
        self.send(vec![0x08])?;
        loop {
            let reply = self.next()?;
            refusal(&reply)?;
            if reply[0] == 0x09 {
                // The other players' skins are asked for on this socket.
                if let Ok(socket) = self.socket.try_clone() {
                    let mut skins = skins();
                    *skins = Skins::default();
                    skins.socket = Some(socket);
                }
                return Ok(allowed);
            }
        }
    }

    /// The server list's status from the plugin's MOTD query, in the JSON a
    /// vanilla server answers with.
    fn query_status(&self) -> io::Result<String> {
        self.socket.set_read_timeout(Some(STEP));
        self.socket.send(Message::Text("Accept: motd".into()))?;
        let reply = loop {
            match self.socket.recv()? {
                Some(Message::Text(t)) => break t,
                Some(Message::Binary(_)) => {}
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        };
        // A server with an icon sends it next, as a message of its own.
        let mut icon = None;
        if serde_json::from_str::<serde_json::Value>(&reply)
            .is_ok_and(|r| r["data"]["icon"] == true)
        {
            while let Ok(Some(message)) = self.socket.recv() {
                if let Message::Binary(pixels) = message {
                    icon = Some(pixels);
                    break;
                }
            }
        }
        let protocol = self.shared().protocol;
        Ok(status_json(&reply, protocol, icon.as_deref()).to_string())
    }
}

/// A JSON text component.
fn text(message: &str) -> String {
    serde_json::json!({ "text": message }).to_string()
}

fn str8(w: &mut Writer, s: &str) {
    let bytes = &s.as_bytes()[..s.len().min(255)];
    w.u8(bytes.len() as u8);
    w.data.extend_from_slice(bytes);
}

/// The plugin's ways of saying no: a version mismatch, a refused login, a
/// redirect elsewhere or an error, as the reason the client shows.
fn refusal(reply: &[u8]) -> Result<(), String> {
    let mut r = Reader::new(reply);
    let reason = (|| -> crate::Result<Option<String>> {
        Ok(match r.u8()? {
            // VERSION_MISMATCH: the Eagler protocols, the Minecraft range,
            // then a message.
            0x03 => {
                let n = r.u16()?;
                r.bytes(2 * usize::from(n))?;
                r.u16()?;
                let (min, max) = (r.u16()?, r.u16()?);
                let n = r.u8()?;
                let message = String::from_utf8_lossy(r.bytes(n.into())?).into_owned();
                Some(text(&format!(
                    "{message} (this server takes Minecraft protocols {min} to {max})"
                )))
            }
            // DENY_LOGIN: a JSON component.
            0x06 => {
                let n = r.u16()?;
                Some(String::from_utf8_lossy(r.bytes(n.into())?).into_owned())
            }
            0x0A => {
                let n = r.u16()?;
                let to = String::from_utf8_lossy(r.bytes(n.into())?).into_owned();
                Some(text(&format!("This server sends its players to {to}")))
            }
            // ERROR: a code, then the message (u16-long once the version is
            // settled, u8-long before; tell them apart by what fits).
            0xFF => {
                let code = r.u8()?;
                let rest = r.rest();
                let message = match rest {
                    [a, b, m @ ..] if usize::from(u16::from_be_bytes([*a, *b])) == m.len() => m,
                    [_, m @ ..] => m,
                    [] => &[],
                };
                let message = String::from_utf8_lossy(message);
                Some(match serde_json::from_str::<serde_json::Value>(&message) {
                    Ok(v) if v.is_object() => message.into_owned(),
                    _ if message.is_empty() => text(&format!("Eaglercraft error {code}")),
                    _ => text(&message),
                })
            }
            _ => None,
        })
    })();
    match reason {
        Ok(None) => Ok(()),
        Ok(Some(reason)) => Err(reason),
        Err(_) => Err(text("Unexpected reply from the Eaglercraft server")),
    }
}

/// `{"type":"motd","data":{"motd":[..],"online":n,"max":n,"players":[..]}}`
/// as a vanilla status, with the icon's pixels (64×64, RGBA) as its favicon.
fn status_json(reply: &str, protocol: i32, icon: Option<&[u8]>) -> serde_json::Value {
    let reply: serde_json::Value = serde_json::from_str(reply).unwrap_or_default();
    let data = &reply["data"];
    let motd = data["motd"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let sample: Vec<_> = data["players"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str())
        .map(|name| serde_json::json!({ "name": name, "id": "00000000-0000-0000-0000-000000000000" }))
        .collect();
    let mut status = serde_json::json!({
        "version": { "name": reply["name"].as_str().unwrap_or("Eaglercraft"), "protocol": protocol },
        "players": {
            "online": data["online"].as_i64().unwrap_or(0),
            "max": data["max"].as_i64().unwrap_or(0),
            "sample": sample,
        },
        "description": { "text": motd },
    });
    if let Some(pixels) = icon.filter(|p| p.len() == ICON_BYTES) {
        let png = crate::websocket::base64(&png(64, 64, pixels));
        status["favicon"] = format!("data:image/png;base64,{png}").into();
    }
    status
}

/// The MOTD's icon: 64×64 pixels, four bytes each (red, green, blue, alpha).
const ICON_BYTES: usize = 64 * 64 * 4;

/// `rgba` pixels as a PNG, the form a vanilla status carries its favicon in.
fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    // Each row starts with its filter type: none.
    let mut rows = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        rows.push(0);
        rows.extend_from_slice(row);
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = zlib.write_all(&rows);
    let data = zlib.finish().unwrap_or_default();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut header = [0; 13];
    header[..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    // 8-bit RGBA, deflate, adaptive filtering, no interlace.
    header[8..].copy_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &data);
    chunk(b"IEND", &[]);
    out
}

/// A skin as the plugin carries it (`skin_v2`'s pixels): 64×64, three bytes
/// a pixel, blue, green, then red's top seven bits under alpha's top bit.
const SKIN_BYTES: usize = 64 * 64 * 3;
/// A cape as the plugin carries it: the 23×17 pixels of the 64×32 cape that
/// show, blue, green and red.
const CAPE_BYTES: usize = 23 * 17 * 3;

/// A 64×64 RGBA skin in the plugin's packing.
pub fn pack_skin(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SKIN_BYTES);
    for p in rgba.chunks_exact(4).take(64 * 64) {
        out.extend_from_slice(&[p[2], p[1], (p[0] >> 1) | (p[3] & 0x80)]);
    }
    out.resize(SKIN_BYTES, 0);
    out
}

/// The plugin's packing back as a 64×64 RGBA skin (one bit of alpha).
pub fn unpack_skin(packed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 64 * 4);
    for p in packed.chunks_exact(3).take(64 * 64) {
        let alpha = if p[2] & 0x80 != 0 { 255 } else { 0 };
        out.extend_from_slice(&[(p[2] & 0x7F) << 1, p[1], p[0], alpha]);
    }
    out.resize(64 * 64 * 4, 0);
    out
}

/// Where each of the cape's 23×17 pixels is in the 64×32 cape
/// (SkinConverter.convertCape64x32RGBAto23x17RGB): the first 22 columns of
/// the top 17 rows, and column 22 of rows 11 to 21 in that column's rows 6
/// to 16.
fn cape_pixels() -> impl Iterator<Item = (usize, usize)> {
    let main = (0..17).flat_map(|y| (0..22).map(move |x| (y * 64 + x, y * 23 + x)));
    let edge = (0..11).map(|y| ((y + 11) * 64 + 22, (y + 6) * 23 + 22));
    main.chain(edge)
}

/// A 64×32 RGBA cape in the plugin's packing.
pub fn pack_cape(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0; CAPE_BYTES];
    for (from, to) in cape_pixels() {
        if let Some(p) = rgba.get(from * 4..from * 4 + 4) {
            out[to * 3..to * 3 + 3].copy_from_slice(&[p[2], p[1], p[0]]);
        }
    }
    out
}

/// The plugin's packing back as a 64×32 RGBA cape.
pub fn unpack_cape(packed: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 64 * 32 * 4];
    for (to, from) in cape_pixels() {
        if let Some(p) = packed.get(from * 3..from * 3 + 3) {
            out[to * 4..to * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    out
}

/// The profile data the login sends: `skin_v2` (custom: 2, the model, the
/// pixels) and `cape_v1` (custom: 2, the pixels).
static LOOK: Mutex<Option<Vec<(&'static str, Vec<u8>)>>> = Mutex::new(None);

/// What this client wears on Eaglercraft servers from its next login: a
/// 64×64 RGBA skin with slim (three-pixel) arms or not, and a 64×32 cape.
/// `None` leaves the plugin's default skin.
pub fn set_look(skin: Option<(&[u8], bool)>, cape: Option<&[u8]>) {
    let entries = look_entries(skin, cape);
    *horizon_os::sync::lock(&LOOK).unwrap_or_else(|e| e.into_inner()) =
        (!entries.is_empty()).then_some(entries);
}

fn look_entries(skin: Option<(&[u8], bool)>, cape: Option<&[u8]>) -> Vec<(&'static str, Vec<u8>)> {
    let mut entries = Vec::new();
    if let Some((rgba, slim)) = skin {
        let mut data = vec![2, u8::from(slim)];
        data.extend_from_slice(&pack_skin(rgba));
        entries.push(("skin_v2", data));
    }
    if let Some(rgba) = cape {
        let mut data = vec![2];
        data.extend_from_slice(&pack_cape(rgba));
        entries.push(("cape_v1", data));
    }
    entries
}

/// PROFILE_DATA: a count, then each entry's type (u8-long) and data
/// (u16-long).
fn profile_data() -> Option<Vec<u8>> {
    let look = horizon_os::sync::lock(&LOOK).unwrap_or_else(|e| e.into_inner());
    look.as_deref().map(encode_profile)
}

fn encode_profile(entries: &[(&'static str, Vec<u8>)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(0x07);
    w.u8(entries.len() as u8);
    for (kind, data) in entries {
        str8(&mut w, kind);
        w.u16(data.len() as u16);
        w.data.extend_from_slice(data);
    }
    w.data
}

/// Another player's skin, as the plugin gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skin {
    /// One of Eaglercraft's preset skins (EnumPresetSkins: 0 Default Steve,
    /// 1 Default Alex...).
    Preset(u32),
    /// 64×64 RGBA.
    Custom { rgba: Vec<u8>, slim: bool },
}

/// Another player's skin and cape (64×32 RGBA, `None` for the preset ones,
/// which have no picture here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherLook {
    pub skin: Skin,
    pub cape: Option<Vec<u8>>,
}

/// The skins asked of the plugin and those it sent.
#[derive(Default)]
struct Skins {
    socket: Option<Box<dyn Messages>>,
    next_request: i32,
    /// Request id → the player it asked for.
    pending: HashMap<i32, u128>,
    /// `None` while asked.
    looks: HashMap<u128, Option<Arc<OtherLook>>>,
}

fn skins() -> std::sync::MutexGuard<'static, Skins> {
    static SKINS: std::sync::OnceLock<Mutex<Skins>> = std::sync::OnceLock::new();
    horizon_os::sync::lock(SKINS.get_or_init(Mutex::default)).unwrap_or_else(|e| e.into_inner())
}

/// What a player of the Eaglercraft server the client is on wears: asked of
/// the plugin the first time (CPacketGetOtherTexturesV5EAG), `None` until it
/// answers, and always on other servers.
pub fn other_look(uuid: u128) -> Option<Arc<OtherLook>> {
    let mut skins = skins();
    if let Some(look) = skins.looks.get(&uuid) {
        return look.clone();
    }
    let socket = skins.socket.as_ref()?;
    let request = skins.next_request;
    let mut w = Writer::new();
    w.u8(0xEE);
    w.u8(0x03);
    w.varint(request);
    w.i64((uuid >> 64) as i64);
    w.i64(uuid as i64);
    if socket.send(Message::Binary(w.data)).is_err() {
        return None;
    }
    skins.next_request = request.wrapping_add(1);
    skins.pending.insert(request, uuid);
    skins.looks.insert(uuid, None);
    None
}

/// The plugin's messages after their `0xEE`: one packet, or `0xFF`, a count
/// and that many packets, each after its length.
fn plugin_messages(data: &[u8]) {
    for_each_message(data, plugin_message);
}

fn for_each_message(data: &[u8], mut each: impl FnMut(&[u8])) {
    let mut r = Reader::new(data);
    if data.first() == Some(&0xFF) {
        let _ = (|| -> crate::Result<()> {
            r.u8()?;
            for _ in 0..r.varint()? {
                let n = usize::try_from(r.varint()?).unwrap_or(0);
                each(r.bytes(n)?);
            }
            Ok(())
        })();
    } else {
        each(data);
    }
}

/// One plugin message: SPacketOtherTexturesV5EAG (the request id, the skin
/// id and the cape id, a negative one followed by its pixels) is the one
/// read; the rest (voice, notifications, its pause menu...) are dropped.
fn plugin_message(data: &[u8]) {
    if let Some((request, look)) = textures(data) {
        let mut skins = skins();
        if let Some(uuid) = skins.pending.remove(&request) {
            skins.looks.insert(uuid, Some(Arc::new(look)));
        }
    }
}

fn textures(data: &[u8]) -> Option<(i32, OtherLook)> {
    let mut r = Reader::new(data);
    (|| -> crate::Result<Option<(i32, OtherLook)>> {
        if r.u8()? != 0x05 {
            return Ok(None);
        }
        let request = r.varint()?;
        let skin = r.varint()?;
        let cape = r.varint()?;
        let skin = if skin < 0 {
            // -(model id) - 1, the model's top bit set: 1 is Alex's arms.
            let model = (-(skin + 1)) & 0x7F;
            Skin::Custom {
                rgba: unpack_skin(r.bytes(SKIN_BYTES)?),
                slim: model == 1,
            }
        } else {
            Skin::Preset(skin as u32)
        };
        let cape = (cape < 0)
            .then(|| r.bytes(CAPE_BYTES).map(unpack_cape))
            .transpose()?;
        Ok(Some((request, OtherLook { skin, cape })))
    })()
    .ok()
    .flatten()
}

impl Read for EaglerSocket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut shared = self.shared();
            if !shared.incoming.is_empty() {
                let n = out.len().min(shared.incoming.len());
                out[..n].copy_from_slice(&shared.incoming[..n]);
                shared.incoming.drain(..n);
                return Ok(n);
            }
            match shared.phase {
                Phase::Closed => return Ok(0),
                Phase::Play | Phase::Acknowledge => {}
                // Before the login only answers made here come.
                _ => return Err(io::ErrorKind::WouldBlock.into()),
            }
            drop(shared);
            match self.socket.recv()? {
                None => return Ok(0),
                // The plugin's own messages (skins, voice, its pause menu)
                // start with 0xEE, which no game packet id does.
                Some(Message::Binary(data)) if data.first() == Some(&0xEE) => {
                    plugin_messages(&data[1..]);
                }
                Some(Message::Binary(data)) if !data.is_empty() => {
                    let data = if self.shared().protocol == crate::translate::modern::PROTOCOL_26_2
                    {
                        match crate::translate::modern::rewrite_clientbound_from_26_2(&data) {
                            Some(data) => data,
                            None => continue,
                        }
                    } else {
                        data
                    };
                    let framed = frame::prefixed(&data);
                    self.shared().incoming.extend_from_slice(&framed);
                }
                Some(_) => {}
            }
        }
    }
}

impl Write for EaglerSocket {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.outgoing.extend_from_slice(data);
        while let Some(body) = frame::split(&mut self.outgoing).map_err(io::Error::other)? {
            self.handle(body)?;
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
    use crate::connection::Connection;
    use std::collections::VecDeque;

    /// The plugin's side: what the client sent, and replies queued for it.
    #[derive(Default)]
    struct Server {
        sent: Mutex<Vec<Message>>,
        replies: Mutex<VecDeque<Message>>,
    }
    struct Fake(Arc<Server>);
    impl Messages for Fake {
        fn send(&self, message: Message) -> io::Result<()> {
            self.0.sent.lock().unwrap().push(message);
            Ok(())
        }
        fn recv(&self) -> io::Result<Option<Message>> {
            Ok(self.0.replies.lock().unwrap().pop_front())
        }
        fn set_read_timeout(&self, _: Option<Duration>) {}
        fn shutdown(&self) {}
        fn try_clone(&self) -> io::Result<Box<dyn Messages>> {
            Ok(Box::new(Fake(self.0.clone())))
        }
    }

    fn connection(replies: Vec<Message>) -> (Connection, Arc<Server>) {
        let server = Arc::new(Server::default());
        server.replies.lock().unwrap().extend(replies);
        let c = Connection::eagler(Box::new(Fake(server.clone()))).unwrap();
        (c, server)
    }

    fn intention(intent: i32) -> Vec<u8> {
        handshake::Serverbound::Intention(handshake::Intention {
            protocol: 777,
            host: "play.example.net".into(),
            port: 443,
            intent,
        })
        .encode()
    }

    #[test]
    fn logs_in_through_the_plugin() {
        let uuid = crate::crypto::offline_uuid("Steve");
        let mut allow = vec![0x05, 5];
        allow.extend_from_slice(b"Steve");
        allow.extend_from_slice(&uuid.to_be_bytes());
        allow.extend_from_slice(&[0, 0]);
        let (mut c, server) = connection(vec![
            // SERVER_VERSION: Eagler 5, Minecraft 777, brand, version, no
            // auth, no salt, no nickname selection.
            Message::Binary(vec![0x02, 0, 5, 0x03, 0x09, 1, b'E', 1, b'1', 0, 0, 0, 0]),
            Message::Binary(allow),
            Message::Binary(vec![0x09]),
            // The first configuration packet, and the plugin's own message.
            Message::Binary(vec![0xEE, 1, 2]),
            Message::Binary(vec![0x03, 7]),
        ]);
        c.send(&intention(handshake::LOGIN)).unwrap();
        c.send(
            &login::Serverbound::Hello(login::ServerHello {
                name: "Steve".into(),
                uuid,
            })
            .encode(),
        )
        .unwrap();
        let (id, payload) = c.recv().unwrap();
        match login::Clientbound::decode(id, &payload).unwrap() {
            login::Clientbound::LoginFinished(p) => {
                assert_eq!((p.profile.name.as_str(), p.profile.uuid), ("Steve", uuid));
            }
            p => panic!("{p:?}"),
        }
        c.send(&login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode())
            .unwrap();
        assert_eq!(c.recv().unwrap(), (3, vec![7]));
        c.send(&[2, 1, 2, 3]).unwrap();
        let sent = server.sent.lock().unwrap();
        let Message::Binary(hello) = &sent[0] else {
            panic!()
        };
        // Eagler 5 offered with Minecraft 777 (0x0309) then 776 (0x0308).
        assert_eq!(&hello[..10], &[0x01, 0x02, 0, 1, 0, 5, 0, 2, 0x03, 0x09]);
        assert_eq!(&hello[10..12], &[0x03, 0x08]);
        assert_eq!(
            sent[1..],
            [
                Message::Binary(
                    [&[0x04, 5][..], b"Steve", &[7], b"default", &[0, 0, 0, 0, 0]].concat()
                ),
                Message::Binary(vec![0x08]),
                // The acknowledgement stayed here; the game packet went as
                // its own message, without its length.
                Message::Binary(vec![2, 1, 2, 3]),
            ]
        );
    }

    #[test]
    fn joins_a_26_2_host_and_translates_both_directions() {
        let uuid = crate::crypto::offline_uuid("Steve");
        let mut allow = vec![0x05, 5];
        allow.extend_from_slice(b"Steve");
        allow.extend_from_slice(&uuid.to_be_bytes());
        allow.extend_from_slice(&[0, 0]);
        let (mut c, server) = connection(vec![
            // SERVER_VERSION: Eagler 5, Minecraft 776, brand, version.
            Message::Binary(vec![0x02, 0, 5, 0x03, 0x08, 1, b'E', 1, b'1', 0, 0, 0, 0]),
            Message::Binary(allow),
            Message::Binary(vec![0x09]),
            // A 26.2 play packet whose id 26.3 shifted by one.
            Message::Binary(vec![37, 0xAA]),
        ]);
        c.send(&intention(handshake::LOGIN)).unwrap();
        c.send(
            &login::Serverbound::Hello(login::ServerHello {
                name: "Steve".into(),
                uuid,
            })
            .encode(),
        )
        .unwrap();
        let (id, payload) = c.recv().unwrap();
        assert!(matches!(
            login::Clientbound::decode(id, &payload).unwrap(),
            login::Clientbound::LoginFinished(_)
        ));
        c.send(&login::Serverbound::LoginAcknowledged(crate::packets::Empty).encode())
            .unwrap();
        assert_eq!(
            server.replies.lock().unwrap().len(),
            1,
            "the play reply is still queued"
        );
        // 26.2's id 37 is this world's 38.
        assert_eq!(c.recv().unwrap(), (38, vec![0xAA]));

        // 26.3's punch reaches the 26.2 host as a swing of the main hand.
        c.send(&[46]).unwrap();
        assert_eq!(
            server.sent.lock().unwrap().last(),
            Some(&Message::Binary(vec![63, 0]))
        );
    }

    #[test]
    fn a_refusal_is_the_login_disconnect() {
        let reason = br#"{"text":"Banned"}"#;
        let mut deny = vec![0x06, 0, reason.len() as u8];
        deny.extend_from_slice(reason);
        let (mut c, _) = connection(vec![
            Message::Binary(vec![0x02, 0, 5, 0x03, 0x09, 0, 0, 0, 0, 0, 0]),
            Message::Binary(deny),
        ]);
        c.send(&intention(handshake::LOGIN)).unwrap();
        c.send(
            &login::Serverbound::Hello(login::ServerHello {
                name: "Steve".into(),
                uuid: 1,
            })
            .encode(),
        )
        .unwrap();
        let (id, payload) = c.recv().unwrap();
        match login::Clientbound::decode(id, &payload).unwrap() {
            login::Clientbound::Disconnect(d) => assert_eq!(d.reason_json.as_bytes(), reason),
            p => panic!("{p:?}"),
        }
        assert!(c.recv().is_err());
    }

    #[test]
    fn status_comes_from_the_motd_query() {
        let (mut c, server) = connection(vec![Message::Text(
            r#"{"name":"Lobby","type":"motd","data":{"motd":["A","B"],"online":3,"max":20,"players":["Alex"]}}"#
                .into(),
        )]);
        c.send(&intention(handshake::STATUS)).unwrap();
        c.send(&status::Serverbound::StatusRequest(crate::packets::Empty).encode())
            .unwrap();
        let (id, payload) = c.recv().unwrap();
        let status::Clientbound::StatusResponse(s) =
            status::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!()
        };
        let json: serde_json::Value = serde_json::from_str(&s.json).unwrap();
        assert_eq!(json["description"]["text"], "A\nB");
        assert_eq!(json["players"]["online"], 3);
        assert_eq!(json["players"]["sample"][0]["name"], "Alex");
        assert_eq!(json["version"]["protocol"], 777);
        c.send(&status::Serverbound::PingRequest(status::Ping { time: 42 }).encode())
            .unwrap();
        let (id, payload) = c.recv().unwrap();
        assert!(matches!(
            status::Clientbound::decode(id, &payload).unwrap(),
            status::Clientbound::PongResponse(status::Ping { time: 42 })
        ));
        assert_eq!(
            server.sent.lock().unwrap()[..],
            [Message::Text("Accept: motd".into())]
        );
    }

    #[test]
    fn the_motd_icon_becomes_the_favicon() {
        let pixels: Vec<u8> = (0..ICON_BYTES).map(|i| i as u8).collect();
        let (mut c, _server) = connection(vec![
            Message::Text(r#"{"type":"motd","data":{"motd":["A"],"icon":true}}"#.into()),
            Message::Binary(pixels.clone()),
        ]);
        c.send(&intention(handshake::STATUS)).unwrap();
        c.send(&status::Serverbound::StatusRequest(crate::packets::Empty).encode())
            .unwrap();
        let (id, payload) = c.recv().unwrap();
        let status::Clientbound::StatusResponse(s) =
            status::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!()
        };
        let json: serde_json::Value = serde_json::from_str(&s.json).unwrap();
        let favicon = json["favicon"].as_str().unwrap();
        let encoded = favicon.strip_prefix("data:image/png;base64,").unwrap();
        assert_eq!(encoded, crate::websocket::base64(&png(64, 64, &pixels)));

        // The PNG: its signature, a 64×64 RGBA header, and the rows behind filter bytes.
        let png = png(64, 64, &pixels);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..29], &[0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
        let idat = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let mut rows = Vec::new();
        flate2::read::ZlibDecoder::new(&png[41..41 + idat])
            .read_to_end(&mut rows)
            .unwrap();
        assert_eq!(rows.len(), 64 * (1 + 64 * 4));
        for (row, expected) in rows.chunks(1 + 64 * 4).zip(pixels.chunks(64 * 4)) {
            assert_eq!((row[0], &row[1..]), (0, expected));
        }
        assert!(png.ends_with(b"IEND\xae\x42\x60\x82"));
    }

    #[test]
    fn skins_and_capes_pack_as_the_plugin_does() {
        let mut skin = vec![0u8; 64 * 64 * 4];
        skin[..8].copy_from_slice(&[200, 100, 50, 255, 9, 8, 7, 0]);
        let packed = pack_skin(&skin);
        assert_eq!(packed.len(), 12288);
        // Blue, green, red's top seven bits under alpha's top bit.
        assert_eq!(&packed[..6], &[50, 100, 100 | 0x80, 7, 8, 4]);
        let back = unpack_skin(&packed);
        assert_eq!(&back[..8], &[200, 100, 50, 255, 8, 8, 7, 0]);

        let mut cape = vec![0u8; 64 * 32 * 4];
        // (22, 11) lands at (22, 6) of the 23×17 picture.
        let at = (11 * 64 + 22) * 4;
        cape[at..at + 4].copy_from_slice(&[1, 2, 3, 255]);
        let packed = pack_cape(&cape);
        assert_eq!(packed.len(), 1173);
        let to = (6 * 23 + 22) * 3;
        assert_eq!(&packed[to..to + 3], &[3, 2, 1]);
        assert_eq!(&unpack_cape(&packed)[at..at + 4], &[1, 2, 3, 255]);
    }

    #[test]
    fn other_players_textures_are_read() {
        let mut w = Writer::new();
        w.u8(0x05);
        w.varint(4);
        // A custom skin with Alex's arms (model 1, its top bit set), a
        // preset cape.
        w.varint(-(0x81) - 1);
        w.varint(3);
        w.data.extend_from_slice(&pack_skin(&[255; 64 * 64 * 4]));
        // As a multi-packet: 0xFF, one packet after its length.
        let mut multi = vec![0xFF, 1];
        let mut len = Writer::new();
        len.varint(w.data.len() as i32);
        multi.extend_from_slice(&len.data);
        multi.extend_from_slice(&w.data);
        let mut read = Vec::new();
        for_each_message(&multi, |m| read.push(textures(m)));
        let [Some((4, look))] = &read[..] else {
            panic!("{read:?}")
        };
        assert_eq!(look.cape, None);
        let Skin::Custom { rgba, slim } = &look.skin else {
            panic!()
        };
        assert!(*slim);
        assert_eq!(&rgba[..4], &[254, 255, 255, 255]);
    }

    #[test]
    fn profile_data_carries_the_skin_and_cape() {
        let data = encode_profile(&look_entries(
            Some((&[0; 64 * 64 * 4], true)),
            Some(&[0; 64 * 32 * 4]),
        ));
        assert_eq!(&data[..3], &[0x07, 2, 7]);
        assert_eq!(&data[3..10], b"skin_v2");
        assert_eq!(&data[10..14], &[0x30, 0x02, 2, 1]);
        let cape = 10 + 2 + 12290;
        assert_eq!(&data[cape..cape + 8], b"\x07cape_v1");
        assert_eq!(&data[cape + 8..cape + 11], &[0x04, 0x96, 2]);
        assert_eq!(data.len(), cape + 10 + 1174);
    }
}
