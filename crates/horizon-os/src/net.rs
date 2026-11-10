//! A server connection on the web, where pages have no TCP: a WebSocket to the
//! server's own port (Horizon's servers take WebSockets beside Minecraft's
//! TCP), read and written as a blocking byte stream.
//!
//! The socket lives on the page, whose thread hands the browser its events;
//! the threads that read it are workers, which wait for its bytes. A worker
//! asks the page to open, flush or close a socket by a message. What threads
//! write goes into one queue the page sends from, so the bytes keep their
//! order whichever thread wrote them.
//!
//! A socket opened by URL (an Eaglercraft server's) is read and written a
//! message at a time instead ([`WebSocket::send`], [`WebSocket::recv`]).
//!
//! Shared worlds (Eaglercraft's LAN worlds) are reached through a relay, an
//! EaglerSPRelay: the host's page keeps a WebSocket to it and gets a join
//! code ([`SharedWorld`]); a player who joins with the code opens a WebRTC
//! data channel to the host, which the relay only introduces
//! ([`WebSocket::connect_shared`]). The channel then reads and writes like
//! the server's WebSocket: the game's bytes, cut in pieces of at most 16 KiB.

use crate::time::Duration;
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const sockets = new Map();
let onEvent = null, take = null, onListen = null;
function open(id, url, host, port, code, relays) {
    if (code) return joinShared(id, code, relays);
    if (!url) {
        const scheme = location.protocol === "https:" ? "wss" : "ws";
        const name = host.includes(":") ? `[${host}]` : host;
        url = `${scheme}://${name}:${port}/`;
    }
    let ws;
    try {
        ws = new WebSocket(url);
    } catch (e) {
        onEvent(id, 2, null, String(e.message ?? e));
        return;
    }
    ws.binaryType = "arraybuffer";
    sockets.set(id, {
        ready: () => ws.readyState === WebSocket.OPEN,
        send: (data) => ws.send(data),
        close: () => ws.close(),
    });
    ws.onopen = () => {
        onEvent(id, 0, null, "");
        flush(id);
    };
    ws.onmessage = ({ data }) => {
        if (data instanceof ArrayBuffer) onEvent(id, 1, new Uint8Array(data), "");
        else onEvent(id, 3, null, data);
    };
    ws.onclose = ({ code, reason }) => {
        sockets.delete(id);
        onEvent(id, 2, null, reason || (code === 1006 ? "Connection refused" : `closed (${code})`));
    };
}
function flush(id) {
    const socket = sockets.get(id);
    if (socket && socket.ready()) {
        for (const data of take(id)) socket.send(data);
    }
}
function close(id) {
    const socket = sockets.get(id);
    sockets.delete(id);
    socket?.close();
}

// EaglerSPRelay's packets: an id, then the fields; strings are ASCII after
// a u8 length, descriptions and candidates bytes after a u16 one.
const ascii = (bytes) => new TextDecoder("latin1").decode(bytes);
const str8 = (s) => [s.length & 255, ...new TextEncoder().encode(s.slice(0, 255))];
function bytes16(s) {
    const b = new TextEncoder().encode(s);
    return [b.length >> 8, b.length & 255, ...b];
}
const packet = (id, ...fields) => new Uint8Array([id, ...fields.flat()]);
function readStr8(b, o) {
    return [ascii(b.subarray(o + 1, o + 1 + b[o])), o + 1 + b[o]];
}
function readBytes16(b, o) {
    const n = (b[o] << 8) | b[o + 1];
    return [ascii(b.subarray(o + 2, o + 2 + n)), o + 2 + n];
}
// RelayPacket01ICEServers: a u16 count of 'S' (STUN) or 'T' (TURN, with a
// user and a password) servers.
function iceServers(b) {
    const servers = [];
    let o = 3;
    for (let n = (b[1] << 8) | b[2]; n > 0; n--) {
        const turn = b[o++] === 84;
        let url, user, pass;
        [url, o] = readBytes16(b, o);
        [user, o] = readStr8(b, o);
        [pass, o] = readStr8(b, o);
        servers.push(turn ? { urls: url, username: user, credential: pass } : { urls: url });
    }
    return servers;
}
// The whole description once the candidates are in it: the relay passes one
// description and one candidate message each way, so candidates ride in the
// description and the candidate messages carry none.
async function described(pc, description) {
    await pc.setLocalDescription(description);
    if (pc.iceGatheringState !== "complete") {
        await new Promise((done) => {
            const timer = setTimeout(done, 5000);
            pc.addEventListener("icegatheringstatechange", () => {
                if (pc.iceGatheringState === "complete") {
                    clearTimeout(timer);
                    done();
                }
            });
        });
    }
    const d = pc.localDescription;
    return JSON.stringify({ type: d.type, sdp: d.sdp });
}
// A data channel as a socket: binary pieces of at most 16 KiB, held back
// while the browser's buffer is full.
function channelSocket(id, channel, pc) {
    const queue = [];
    const pump = () => {
        while (queue.length && channel.readyState === "open" && channel.bufferedAmount < 4 << 20) {
            channel.send(queue.shift());
        }
    };
    channel.binaryType = "arraybuffer";
    channel.bufferedAmountLowThreshold = 1 << 20;
    channel.onbufferedamountlow = pump;
    channel.onmessage = ({ data }) => {
        if (data instanceof ArrayBuffer) onEvent(id, 1, new Uint8Array(data), "");
    };
    channel.onclose = () => {
        sockets.delete(id);
        pc.close();
        onEvent(id, 2, null, "The shared world closed the connection");
    };
    sockets.set(id, {
        ready: () => channel.readyState === "open",
        send(data) {
            if (typeof data === "string") return;
            for (let i = 0; i < data.length; i += 16384) queue.push(data.subarray(i, i + 16384));
            pump();
        },
        close() {
            channel.close();
            pc.close();
        },
    });
}
// Joining a shared world: each relay in turn until one knows the code.
function joinShared(id, code, relays) {
    let next = 0, why = "No relay could be reached";
    const fail = (reason) => onEvent(id, 2, null, reason);
    const attempt = () => {
        if (next >= relays.length) return fail(why);
        let ws;
        try {
            ws = new WebSocket(relays[next++]);
        } catch (e) {
            return attempt();
        }
        ws.binaryType = "arraybuffer";
        let pc = null, opened = false, answered = false, settled = false, retry = true;
        sockets.set(id, { ready: () => false, send() {}, close() { settled = true; ws.close(); pc?.close(); } });
        const timer = setTimeout(() => {
            if (!opened) {
                retry = false;
                why = "Could not reach the shared world (timed out)";
                ws.close();
                pc?.close();
            }
        }, 25000);
        // RelayPacket05ClientSuccess once the channel is open and the host's
        // candidate message came (the relay wants it in that order).
        const succeed = () => {
            if (opened && answered && !settled) {
                settled = true;
                ws.send(packet(0x05, str8("")));
                ws.close();
            }
        };
        ws.onopen = () => ws.send(packet(0x00, [2, 1], str8(code)));
        ws.onmessage = async ({ data }) => {
            const b = new Uint8Array(data);
            try {
                if (b[0] === 0x01) {
                    pc = new RTCPeerConnection({ iceServers: iceServers(b) });
                    const channel = pc.createDataChannel("horizon", { ordered: true });
                    channel.onopen = () => {
                        opened = true;
                        clearTimeout(timer);
                        channelSocket(id, channel, pc);
                        onEvent(id, 0, null, "");
                        flush(id);
                        succeed();
                    };
                    const offer = await described(pc, await pc.createOffer());
                    ws.send(packet(0x04, str8(""), bytes16(offer)));
                } else if (b[0] === 0x04) {
                    const [description] = readBytes16(b, readStr8(b, 1)[1]);
                    await pc.setRemoteDescription(JSON.parse(description));
                    ws.send(packet(0x03, str8(""), bytes16("[]")));
                } else if (b[0] === 0x03) {
                    answered = true;
                    succeed();
                } else if (b[0] === 0xfe && !opened) {
                    // RelayPacketFEDisconnectClient: the id, a code, the reason.
                    const o = readStr8(b, 1)[1];
                    retry = false;
                    why = readBytes16(b, o + 1)[0] || "The shared world refused the connection";
                } else if (b[0] === 0xff) {
                    // RelayPacketFFErrorCode: 5 is a code this relay doesn't know.
                    retry = b[1] === 5 || b[1] === 4;
                    why = b[1] === 5 ? "No shared world has that code" : readBytes16(b, 2)[0];
                }
            } catch (e) {
                retry = false;
                why = String(e.message ?? e);
                ws.close();
            }
        };
        ws.onclose = () => {
            if (opened || settled) return;
            clearTimeout(timer);
            pc?.close();
            if (retry) attempt();
            else fail(why);
        };
    };
    attempt();
}
// Hosting a shared world: the first relay that gives a code.
const listeners = new Map();
function listen(lid, name, relays) {
    let next = 0, closed = false, ws = null;
    const peers = new Map();
    listeners.set(lid, {
        close() {
            closed = true;
            ws?.close();
            for (const p of peers.values()) if (!p.open) p.pc.close();
        },
    });
    const attempt = () => {
        if (closed) return;
        if (next >= relays.length) return onListen(lid, 2, "No relay could be reached");
        try {
            ws = new WebSocket(relays[next++]);
        } catch (e) {
            return attempt();
        }
        ws.binaryType = "arraybuffer";
        let servers = [], code = null;
        ws.onopen = () => ws.send(packet(0x00, [1, 1], str8(name)));
        ws.onmessage = async ({ data }) => {
            const b = new Uint8Array(data);
            try {
                if (b[0] === 0x00) {
                    code = readStr8(b, 3)[0];
                    console.info(`Horizon: shared world ${code} on ${ws.url}`);
                    onListen(lid, 0, code);
                } else if (b[0] === 0x01) {
                    servers = iceServers(b);
                } else if (b[0] === 0x02) {
                    const [peer] = readStr8(b, 1);
                    const pc = new RTCPeerConnection({ iceServers: servers });
                    const p = { pc, open: false };
                    peers.set(peer, p);
                    pc.ondatachannel = ({ channel }) => {
                        const opened = () => {
                            p.open = true;
                            peers.delete(peer);
                            const id = onListen(lid, 1, peer);
                            channelSocket(id, channel, pc);
                            onEvent(id, 0, null, "");
                        };
                        if (channel.readyState === "open") opened();
                        else channel.onopen = opened;
                    };
                } else if (b[0] === 0x04) {
                    const [peer, o] = readStr8(b, 1);
                    const p = peers.get(peer);
                    if (!p) return;
                    await p.pc.setRemoteDescription(JSON.parse(readBytes16(b, o)[0]));
                    const answer = await described(p.pc, await p.pc.createAnswer());
                    ws.send(packet(0x04, str8(peer), bytes16(answer)));
                } else if (b[0] === 0x03) {
                    ws.send(packet(0x03, str8(readStr8(b, 1)[0]), bytes16("[]")));
                } else if (b[0] === 0x06 || b[0] === 0xfe) {
                    const [peer] = readStr8(b, 1);
                    const p = peers.get(peer);
                    if (p && !p.open) p.pc.close();
                    peers.delete(peer);
                }
            } catch (e) {
                console.warn("Horizon: shared world peer:", e);
            }
        };
        ws.onclose = ({ code: status, reason }) => {
            if (closed) return;
            console.warn(`Horizon: the relay closed the shared world (${status} ${reason})`);
            if (code === null) attempt();
            else onListen(lid, 2, "The relay closed the shared world");
        };
    };
    attempt();
}
function onPage(data) {
    if (data.socket === "open") open(data.id, data.url, data.host, data.port, data.code, data.relays);
    else if (data.socket === "flush") flush(data.id);
    else if (data.socket === "listen") listen(data.id, data.name, data.relays);
    else if (data.socket === "unlisten") {
        listeners.get(data.id)?.close();
        listeners.delete(data.id);
    } else close(data.id);
}
export function horizon_socket_install(events, taker, listening) {
    onEvent = events;
    take = taker;
    onListen = listening;
    // What workers ask of the page arrives at the worker objects the page
    // made (workers.rs), which hand it here.
    globalThis.horizonSocket = onPage;
}
// A worker's blocking GET (workers may make synchronous requests): the body
// of a 2xx answer, or null. An https page asks for https.
export function horizon_http_get(url) {
    if (location.protocol === "https:") url = url.replace(/^http:/, "https:");
    try {
        const xhr = new XMLHttpRequest();
        xhr.open("GET", url, false);
        xhr.responseType = "arraybuffer";
        xhr.send();
        return xhr.status >= 200 && xhr.status < 300 ? new Uint8Array(xhr.response) : null;
    } catch (e) {
        return null;
    }
}
export function horizon_socket_ask(data) {
    if (typeof window === "undefined") self.postMessage(data);
    else onPage(data);
}
"#)]
extern "C" {
    fn horizon_socket_install(events: &JsValue, take: &JsValue, listening: &JsValue);
    fn horizon_socket_ask(data: &JsValue);
    fn horizon_http_get(url: &str) -> JsValue;
}

/// One WebSocket message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Binary(Vec<u8>),
    Text(String),
}

/// What the page knows of a socket, and what its threads wrote.
#[derive(Default)]
struct State {
    open: bool,
    /// Why it closed, once it has.
    closed: Option<String>,
    incoming: VecDeque<Message>,
    /// How much of the first incoming message a byte read has taken.
    taken: usize,
    outgoing: Vec<Message>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

fn sockets() -> &'static Mutex<HashMap<u32, Arc<Shared>>> {
    static SOCKETS: OnceLock<Mutex<HashMap<u32, Arc<Shared>>>> = OnceLock::new();
    SOCKETS.get_or_init(Mutex::default)
}

fn find(id: u32) -> Option<Arc<Shared>> {
    crate::sync::lock(sockets())
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
}

/// Where a socket goes: a URL, a host and port, or a shared world's join
/// code and the relays to look for it on.
#[derive(Clone, Copy)]
enum Target<'a> {
    Url(&'a str),
    Host(&'a str, u16),
    Shared(&'a str, &'a [String]),
}

fn ask(what: &str, id: u32, target: Option<Target<'_>>) {
    let data = js_sys::Object::new();
    let set = |key: &str, value: JsValue| {
        let _ = js_sys::Reflect::set(&data, &JsValue::from_str(key), &value);
    };
    set("socket", JsValue::from_str(what));
    set("id", JsValue::from(id));
    match target {
        Some(Target::Url(url)) => set("url", JsValue::from_str(url)),
        Some(Target::Host(host, port)) => {
            set("host", JsValue::from_str(host));
            set("port", JsValue::from(port));
        }
        Some(Target::Shared(code, relays)) => {
            set("code", JsValue::from_str(code));
            set("relays", relays_array(relays).into());
        }
        None => {}
    }
    horizon_socket_ask(&data);
}

fn relays_array(relays: &[String]) -> js_sys::Array {
    relays.iter().map(|r| JsValue::from_str(r)).collect()
}

/// Hands the page's socket code the game's side of it. Called on the page
/// before the game starts.
pub(crate) fn install() {
    let events = Closure::<dyn FnMut(u32, u32, JsValue, String)>::new(
        |id: u32, kind: u32, data: JsValue, reason: String| {
            let Some(shared) = find(id) else { return };
            let mut state = crate::sync::lock(&shared.state).unwrap_or_else(|e| e.into_inner());
            match kind {
                0 => state.open = true,
                1 => state
                    .incoming
                    .push_back(Message::Binary(js_sys::Uint8Array::new(&data).to_vec())),
                3 => state.incoming.push_back(Message::Text(reason)),
                _ => {
                    state.closed.get_or_insert(reason);
                    crate::sync::lock(sockets())
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id);
                }
            }
            drop(state);
            shared.ready.notify_all();
        },
    );
    let take = Closure::<dyn FnMut(u32) -> JsValue>::new(|id: u32| {
        let messages = js_sys::Array::new();
        let Some(shared) = find(id) else {
            return messages.into();
        };
        let mut state = crate::sync::lock(&shared.state).unwrap_or_else(|e| e.into_inner());
        for message in state.outgoing.drain(..) {
            messages.push(&match message {
                // A copy outside the shared memory, which `WebSocket.send`
                // refuses.
                Message::Binary(data) => js_sys::Uint8Array::from(&data[..]).into(),
                Message::Text(text) => JsValue::from_str(&text),
            });
        }
        messages.into()
    });
    let listening = Closure::<dyn FnMut(u32, u32, String) -> u32>::new(
        |lid: u32, kind: u32, text: String| -> u32 {
            let Some(listener) = crate::sync::lock(listeners())
                .unwrap_or_else(|e| e.into_inner())
                .get(&lid)
                .cloned()
            else {
                return 0;
            };
            let mut state = crate::sync::lock(&listener).unwrap_or_else(|e| e.into_inner());
            match kind {
                0 => {
                    state.code = Some(text.clone());
                    *crate::sync::lock(&NEWS).unwrap_or_else(|e| e.into_inner()) = Some(Ok(text));
                }
                // A player's channel opened: a socket for the host to accept.
                1 => {
                    let id = next_id();
                    let shared = Arc::new(Shared::default());
                    crate::sync::lock(&shared.state)
                        .unwrap_or_else(|e| e.into_inner())
                        .open = true;
                    crate::sync::lock(sockets())
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id, shared.clone());
                    state.accepted.push_back((id, shared));
                    return id;
                }
                _ => {
                    state.error = Some(text.clone());
                    *crate::sync::lock(&NEWS).unwrap_or_else(|e| e.into_inner()) = Some(Err(text));
                }
            }
            0
        },
    );
    horizon_socket_install(events.as_ref(), take.as_ref(), listening.as_ref());
    events.forget();
    take.forget();
    listening.forget();
}

fn next_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Closes the socket when its last clone goes, as a TCP socket closes.
struct Handle {
    id: u32,
    shared: Arc<Shared>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        close(self.id, &self.shared);
    }
}

fn close(id: u32, shared: &Shared) {
    let mut state = crate::sync::lock(&shared.state).unwrap_or_else(|e| e.into_inner());
    if state.closed.is_none() {
        state.closed = Some("Connection closed".into());
        drop(state);
        shared.ready.notify_all();
        crate::sync::lock(sockets())
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        ask("close", id, None);
    }
}

/// A WebSocket to `host:port`, read and written like a `TcpStream`. Reads
/// wait, so only workers read it.
pub struct WebSocket {
    handle: Arc<Handle>,
    timeout: Mutex<Option<Duration>>,
}

impl WebSocket {
    /// Opens the socket (`wss:` on an `https:` page, else `ws:`) and waits
    /// until it is open. Only on a worker: the page can't wait.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<Self> {
        Self::open(Target::Host(host, port), timeout)
    }

    /// Opens a socket to a `ws:` or `wss:` URL, to read a message at a time.
    pub fn connect_url(url: &str, timeout: Duration) -> io::Result<Self> {
        Self::open(Target::Url(url), timeout)
    }

    /// Joins the shared world with this code, looking for it on each relay
    /// in turn: a data channel to its host, read and written as a stream.
    pub fn connect_shared(code: &str, relays: &[String], timeout: Duration) -> io::Result<Self> {
        Self::open(Target::Shared(code, relays), timeout)
    }

    fn open(target: Target<'_>, timeout: Duration) -> io::Result<Self> {
        if !crate::workers::in_worker() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the page's thread can't wait for a connection",
            ));
        }
        let id = next_id();
        let shared = Arc::new(Shared::default());
        crate::sync::lock(sockets())
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, shared.clone());
        let socket = Self {
            handle: Arc::new(Handle { id, shared }),
            timeout: Mutex::new(None),
        };
        ask("open", id, Some(target));
        let shared = &socket.handle.shared;
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, timed_out) =
            crate::sync::wait_timeout_while(&shared.state, &shared.ready, state, timeout, |s| {
                !s.open && s.closed.is_none()
            });
        if let Some(reason) = &state.closed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                reason.clone(),
            ));
        }
        if timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Connection timed out",
            ));
        }
        drop(state);
        Ok(socket)
    }

    pub fn try_clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            timeout: Mutex::new(*self.timeout.lock().unwrap_or_else(|e| e.into_inner())),
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) {
        *self.timeout.lock().unwrap_or_else(|e| e.into_inner()) = timeout;
    }

    /// Closes the socket: reads then see the end of the stream.
    pub fn shutdown(&self) {
        close(self.handle.id, &self.handle.shared);
    }

    /// Sends one message.
    pub fn send(&self, message: Message) -> io::Result<()> {
        let handle = &self.handle;
        let mut state = crate::sync::lock(&handle.shared.state).unwrap_or_else(|e| e.into_inner());
        if state.closed.is_some() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        // The page sends all that is queued: one request covers the messages
        // written before it gets to it.
        let first = state.outgoing.is_empty();
        state.outgoing.push(message);
        drop(state);
        if first {
            ask("flush", handle.id, None);
        }
        Ok(())
    }

    /// The next whole message, waiting for it as a read does (with the read
    /// timeout); `None` once the socket closed.
    pub fn recv(&self) -> io::Result<Option<Message>> {
        let mut state = self.wait()?;
        state.taken = 0;
        Ok(state.incoming.pop_front())
    }

    /// Puts a message back in front of the incoming ones, after a peek read
    /// told an Eaglercraft guest from a Horizon page.
    pub fn unrecv(&self, message: Message) {
        crate::sync::lock(&self.handle.shared.state)
            .unwrap_or_else(|e| e.into_inner())
            .incoming
            .push_front(message);
    }

    /// The state once a message came or the socket closed.
    fn wait(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        let shared = &self.handle.shared;
        let mut state = crate::sync::lock(&shared.state).unwrap_or_else(|e| e.into_inner());
        if crate::workers::in_worker() {
            let pending = |s: &mut State| s.incoming.is_empty() && s.closed.is_none();
            match *self.timeout.lock().unwrap_or_else(|e| e.into_inner()) {
                None => {
                    while pending(&mut state) {
                        state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                }
                Some(timeout) => {
                    let timed_out;
                    (state, timed_out) = crate::sync::wait_timeout_while(
                        &shared.state,
                        &shared.ready,
                        state,
                        timeout,
                        pending,
                    );
                    if timed_out {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                }
            }
        } else if state.incoming.is_empty() && state.closed.is_none() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(state)
    }
}

impl Read for WebSocket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut state = self.wait()?;
        // A byte stream has no use for text messages.
        while let Some(Message::Text(_)) = state.incoming.front() {
            state.incoming.pop_front();
            if state.incoming.is_empty() {
                drop(state);
                state = self.wait()?;
            }
        }
        let taken = state.taken;
        let Some(Message::Binary(data)) = state.incoming.front() else {
            return Ok(0);
        };
        let n = out.len().min(data.len() - taken);
        out[..n].copy_from_slice(&data[taken..taken + n]);
        if taken + n == data.len() {
            state.incoming.pop_front();
            state.taken = 0;
        } else {
            state.taken += n;
        }
        Ok(n)
    }
}

impl Write for WebSocket {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.send(Message::Binary(data.to_vec()))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A GET over HTTP(S) through the browser (the server must allow the page's
/// origin, as textures.minecraft.net does): the body of a 2xx answer. Only
/// on a worker, which may wait for it; `None` on the page.
pub fn http_get(url: &str) -> Option<Vec<u8>> {
    if !crate::workers::in_worker() {
        return None;
    }
    let body = horizon_http_get(url);
    (!body.is_null()).then(|| js_sys::Uint8Array::new(&body).to_vec())
}

/// What the page heard of a hosted shared world.
#[derive(Default)]
struct Listening {
    code: Option<String>,
    error: Option<String>,
    /// Sockets of players whose channel opened, to accept.
    accepted: VecDeque<(u32, Arc<Shared>)>,
}

fn listeners() -> &'static Mutex<HashMap<u32, Arc<Mutex<Listening>>>> {
    static LISTENERS: OnceLock<Mutex<HashMap<u32, Arc<Mutex<Listening>>>>> = OnceLock::new();
    LISTENERS.get_or_init(Mutex::default)
}

/// The newest code a shared world got, or why it failed, for the window.
static NEWS: Mutex<Option<Result<String, String>>> = Mutex::new(None);

/// A shared world's join code once the relay gave it, or why hosting
/// failed, since the last call: the window tells the player.
pub fn shared_world_news() -> Option<Result<String, String>> {
    crate::sync::lock(&NEWS)
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

/// A world shared through a relay, accepting the players who join it with
/// its code as a `TcpListener` set non-blocking accepts connections. Stops
/// when dropped; players already in stay.
pub struct SharedWorld {
    id: u32,
    state: Arc<Mutex<Listening>>,
}

impl SharedWorld {
    /// Asks the page to share the world (`name` is what the relay lists) on
    /// the first of `relays` that answers. Returns at once: the code comes
    /// later ([`SharedWorld::code`]).
    pub fn open(name: &str, relays: &[String]) -> Self {
        let id = next_id();
        let state = Arc::new(Mutex::new(Listening::default()));
        crate::sync::lock(listeners())
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, state.clone());
        let data = js_sys::Object::new();
        let set = |key: &str, value: JsValue| {
            let _ = js_sys::Reflect::set(&data, &JsValue::from_str(key), &value);
        };
        set("socket", JsValue::from_str("listen"));
        set("id", JsValue::from(id));
        set("name", JsValue::from_str(name));
        set("relays", relays_array(relays).into());
        horizon_socket_ask(&data);
        Self { id, state }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, Listening> {
        crate::sync::lock(&self.state).unwrap_or_else(|e| e.into_inner())
    }

    /// The join code, once the relay gave it.
    pub fn code(&self) -> Option<String> {
        self.state().code.clone()
    }

    /// The next player who joined: their socket and a stand-in address
    /// (the relay doesn't tell); `WouldBlock` when none is waiting.
    pub fn accept(&self) -> io::Result<(WebSocket, std::net::SocketAddr)> {
        let mut state = self.state();
        if let Some((id, shared)) = state.accepted.pop_front() {
            let socket = WebSocket {
                handle: Arc::new(Handle { id, shared }),
                timeout: Mutex::new(None),
            };
            return Ok((socket, std::net::SocketAddr::from(([0, 0, 0, 0], 0))));
        }
        match &state.error {
            Some(e) => Err(io::Error::new(io::ErrorKind::NotConnected, e.clone())),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl Drop for SharedWorld {
    fn drop(&mut self) {
        crate::sync::lock(listeners())
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        ask("unlisten", self.id, None);
    }
}
