//! A blocking connection with vanilla's pipeline: optional AES/CFB8
//! encryption, VarInt21 framing and threshold compression. After login it
//! splits into a reader (for a receive thread) and a shareable writer. It runs
//! over TCP, or over an in-memory pipe between two threads of one process
//! (vanilla's `LocalChannel`, which an integrated server's player uses). A
//! server also takes browsers' WebSockets on its port, and on the web the
//! client reaches servers through one.
use crate::{Error, Result, buf::Reader, crypto::Cfb8, frame};
use horizon_os::time::Duration;
use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

/// One direction of a [`Pipe`]: the bytes written and not read yet, and
/// whether either end closed it.
#[derive(Default)]
struct Channel {
    state: Mutex<(std::collections::VecDeque<u8>, bool)>,
    ready: Condvar,
}
impl Channel {
    fn close(&self) {
        horizon_os::sync::lock(&self.state).unwrap().1 = true;
        self.ready.notify_all();
    }
}
/// The channels of one end; dropping the end's last clone closes them, as
/// a socket closes with its process.
struct End {
    incoming: Arc<Channel>,
    outgoing: Arc<Channel>,
}
impl Drop for End {
    fn drop(&mut self) {
        self.incoming.close();
        self.outgoing.close();
    }
}
/// One end of an in-memory byte pipe: what one end writes the other reads.
pub struct Pipe {
    end: Arc<End>,
    /// The read timeout (`set_read_timeout`).
    timeout: Mutex<Option<Duration>>,
}
impl Pipe {
    /// Two connected ends.
    pub fn pair() -> (Self, Self) {
        let (a, b) = (Arc::new(Channel::default()), Arc::new(Channel::default()));
        let end = |incoming, outgoing| Self {
            end: Arc::new(End { incoming, outgoing }),
            timeout: Mutex::new(None),
        };
        (end(a.clone(), b.clone()), end(b, a))
    }
    fn try_clone(&self) -> Self {
        Self {
            end: self.end.clone(),
            timeout: Mutex::new(*horizon_os::sync::lock(&self.timeout).unwrap()),
        }
    }
    /// Close both directions: reads at either end then see the end of the stream.
    pub fn shutdown(&self) {
        self.end.incoming.close();
        self.end.outgoing.close();
    }
}
impl Read for Pipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let timeout = *self.timeout.lock().unwrap();
        let incoming = &self.end.incoming;
        let state = horizon_os::sync::lock(&incoming.state).unwrap();
        let empty = |s: &mut (std::collections::VecDeque<u8>, bool)| s.0.is_empty() && !s.1;
        let mut state = match timeout {
            None => horizon_os::sync::wait_while(&incoming.state, &incoming.ready, state, empty),
            Some(timeout) => {
                let (state, timed_out) = horizon_os::sync::wait_timeout_while(
                    &incoming.state,
                    &incoming.ready,
                    state,
                    timeout,
                    empty,
                );
                if timed_out {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                state
            }
        };
        let n = out.len().min(state.0.len());
        for (o, b) in out.iter_mut().zip(state.0.drain(..n)) {
            *o = b;
        }
        Ok(n)
    }
}
impl Write for Pipe {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // The page writes here while the other end's worker reads.
        let mut state = horizon_os::sync::lock(&self.end.outgoing.state).unwrap();
        if state.1 {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        state.0.extend(data);
        self.end.outgoing.ready.notify_all();
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A shared world's data channel as an Eaglercraft message socket.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
struct WebMessages {
    socket: horizon_os::net::WebSocket,
    /// The message read to tell an Eaglercraft guest from a Horizon page.
    first: std::sync::Mutex<Option<crate::eagler::Message>>,
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
impl WebMessages {
    fn new(socket: horizon_os::net::WebSocket) -> Self {
        Self {
            socket,
            first: std::sync::Mutex::new(None),
        }
    }

    fn with_first(socket: horizon_os::net::WebSocket, first: crate::eagler::Message) -> Self {
        Self {
            socket,
            first: std::sync::Mutex::new(Some(first)),
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
impl crate::eagler::Messages for WebMessages {
    fn send(&self, message: crate::eagler::Message) -> std::io::Result<()> {
        let message = match message {
            crate::eagler::Message::Binary(data) => horizon_os::net::Message::Binary(data),
            crate::eagler::Message::Text(text) => horizon_os::net::Message::Text(text),
        };
        self.socket.send(message)
    }
    fn recv(&self) -> std::io::Result<Option<crate::eagler::Message>> {
        if let Some(first) = self.first.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Ok(Some(first));
        }
        Ok(self.socket.recv()?.map(|message| match message {
            horizon_os::net::Message::Binary(data) => crate::eagler::Message::Binary(data),
            horizon_os::net::Message::Text(text) => crate::eagler::Message::Text(text),
        }))
    }
    fn set_read_timeout(&self, timeout: Option<horizon_os::time::Duration>) {
        self.socket.set_read_timeout(timeout);
    }
    fn shutdown(&self) {
        self.socket.shutdown();
    }
    fn try_clone(&self) -> std::io::Result<Box<dyn crate::eagler::Messages>> {
        Ok(Box::new(WebMessages::new(self.socket.try_clone())))
    }
}

/// What a connection runs over.
enum Stream {
    Tcp(TcpStream),
    Memory(Pipe),
    /// A browser's connection to this server.
    WebSocket(crate::websocket::ServerSocket),
    /// A browser's connection whose first message was read already (to tell
    /// an Eaglercraft guest from a vanilla client).
    WebSocketPrefixed(crate::websocket::Prefixed),
    /// This page's connection to a server.
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    Web(horizon_os::net::WebSocket),
    /// This page's connection to an Eaglercraft server.
    Eagler(crate::eagler::EaglerSocket),
    /// A shared world's connection to an Eaglercraft guest.
    EaglerServer(crate::eagler_server::EaglerServerSocket),
}
impl Stream {
    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(match self {
            Self::Tcp(s) => Self::Tcp(s.try_clone()?),
            Self::Memory(p) => Self::Memory(p.try_clone()),
            Self::WebSocket(s) => Self::WebSocket(s.try_clone()?),
            Self::WebSocketPrefixed(s) => Self::WebSocketPrefixed(s.try_clone()?),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => Self::Web(s.try_clone()),
            Self::Eagler(s) => Self::Eagler(s.try_clone()?),
            Self::EaglerServer(s) => Self::EaglerServer(s.try_clone()?),
        })
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(timeout),
            Self::Memory(p) => {
                *horizon_os::sync::lock(&p.timeout).unwrap() = timeout;
                Ok(())
            }
            Self::WebSocket(s) => s.stream().set_read_timeout(timeout),
            Self::WebSocketPrefixed(s) => s.set_read_timeout(timeout),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => {
                s.set_read_timeout(timeout);
                Ok(())
            }
            Self::Eagler(s) => {
                s.set_read_timeout(timeout);
                Ok(())
            }
            Self::EaglerServer(s) => {
                s.set_read_timeout(timeout);
                Ok(())
            }
        }
    }
    fn shutdown(&self) {
        match self {
            Self::Tcp(s) => {
                let _ = s.shutdown(Shutdown::Both);
            }
            Self::Memory(p) => p.shutdown(),
            Self::WebSocket(s) => s.shutdown(),
            Self::WebSocketPrefixed(s) => s.shutdown(),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => s.shutdown(),
            Self::Eagler(s) => s.shutdown(),
            Self::EaglerServer(s) => s.shutdown(),
        }
    }
}
impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(out),
            Self::Memory(p) => p.read(out),
            Self::WebSocket(s) => s.read(out),
            Self::WebSocketPrefixed(s) => s.read(out),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => s.read(out),
            Self::Eagler(s) => s.read(out),
            Self::EaglerServer(s) => s.read(out),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(data),
            Self::Memory(p) => p.write(data),
            Self::WebSocket(s) => s.write(data),
            Self::WebSocketPrefixed(s) => s.write(data),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => s.write(data),
            Self::Eagler(s) => s.write(data),
            Self::EaglerServer(s) => s.write(data),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            Self::Memory(p) => p.flush(),
            Self::WebSocket(s) => s.flush(),
            Self::WebSocketPrefixed(s) => s.flush(),
            #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
            Self::Web(s) => s.flush(),
            Self::Eagler(s) => s.flush(),
            Self::EaglerServer(s) => s.flush(),
        }
    }
}

/// Packets received/sent and bytes in/out, for the debug screen.
#[derive(Debug, Default)]
pub struct Counters {
    pub received: AtomicI64,
    pub sent: AtomicI64,
    pub bytes_in: AtomicI64,
    pub bytes_out: AtomicI64,
}

pub struct ConnReader {
    stream: Stream,
    buffer: Vec<u8>,
    decrypt: Option<Cfb8>,
    pub threshold: Option<usize>,
    pub counters: Arc<Counters>,
}
/// Closes a connection from another thread: a read or a write blocked on it fails.
pub struct Closer(Stream);
impl Closer {
    pub fn close(&self) {
        self.0.shutdown();
    }
}

pub struct ConnWriter {
    stream: Stream,
    encrypt: Option<Cfb8>,
    pub threshold: Option<usize>,
    counters: Arc<Counters>,
}
/// `InetAddress.getByName` under Java's default `java.net.preferIPv6Addresses=false`: IPv4
/// addresses first, whatever order the system resolver gives (macOS lists IPv6 first). Vanilla
/// connects to that first address only; the rest stay here as fallbacks.
fn ipv4_first(mut addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addrs.sort_by_key(SocketAddr::is_ipv6);
    addrs
}
/// RFC 8305's connection attempt delay: how long an address has alone before the next
/// one is tried beside it.
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);
/// Both halves before the split.
pub struct Connection {
    pub reader: ConnReader,
    pub writer: ConnWriter,
}
impl Connection {
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
        return Self::connect_web(host, port, timeout);
        #[allow(unreachable_code)]
        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| Error::new(format!("Unknown host {host}: {e}")))?
            .collect();
        Self::connect_addresses(host, addrs, timeout)
    }
    /// Connect through Minecraft's SRV redirect. Status requests still use the
    /// original address in their handshake (`ServerStatusPinger.pingServer`).
    pub fn connect_server(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        Self::connect_server_with_address(host, port, timeout).map(|(connection, _, _)| connection)
    }
    /// `ConnectScreen.startConnecting`: login handshakes use the resolved host
    /// and port, including an SRV redirect, rather than the status request's address.
    pub fn connect_server_with_address(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<(Self, String, u16)> {
        #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
        return Self::connect_web(host, port, timeout).map(|c| (c, host.to_owned(), port));
        #[allow(unreachable_code)]
        let resolved = crate::resolver::resolve(host, port)?;
        let connection = Self::connect_addresses(host, resolved.addresses, timeout)?;
        Ok((connection, resolved.host, resolved.port))
    }
    /// Each address starts once the earlier ones fail or [`ATTEMPT_DELAY`] after the last
    /// start, alongside those still trying, and the first to connect wins: a dead address
    /// in a round-robin set costs 250 ms instead of the whole timeout.
    fn connect_addresses(host: &str, addrs: Vec<SocketAddr>, timeout: Duration) -> Result<Self> {
        let addrs = ipv4_first(addrs);
        let (send, recv) = std::sync::mpsc::channel();
        let mut errors: Vec<Option<std::io::Error>> = addrs.iter().map(|_| None).collect();
        let (mut started, mut pending) = (0, 0);
        loop {
            if let Some(&addr) = addrs.get(started) {
                let (send, index) = (send.clone(), started);
                let spawned = horizon_os::thread::Builder::new()
                    .name("horizon-connect".into())
                    .spawn(move || {
                        let _ = send.send((index, TcpStream::connect_timeout(&addr, timeout)));
                    });
                match spawned {
                    Ok(_) => pending += 1,
                    Err(e) => errors[index] = Some(e),
                }
                started += 1;
            }
            if pending == 0 {
                if started < addrs.len() {
                    continue;
                }
                break;
            }
            let reply = if started < addrs.len() {
                match recv.recv_timeout(ATTEMPT_DELAY) {
                    Ok(reply) => reply,
                    Err(_) => continue,
                }
            } else {
                recv.recv().expect("a sender is held here")
            };
            pending -= 1;
            match reply {
                (_, Ok(stream)) => return Self::from_stream(stream),
                (index, Err(e)) => errors[index] = Some(e),
            }
        }
        // Report the first address's failure: it is the one vanilla tries, and the IPv6
        // fallbacks after it often fail only because the client has no IPv6 route.
        match errors.into_iter().flatten().next() {
            Some(e) => Err(Error::new(format!("Connection refused: {e}"))),
            None => Err(Error::new(format!("Unknown host {host}"))),
        }
    }
    /// The page has no TCP: a WebSocket to the server's port instead.
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    fn connect_web(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        let socket = horizon_os::net::WebSocket::connect(host, port, timeout)
            .map_err(|e| Error::new(format!("Connection refused: {e}")))?;
        Self::over(Stream::Web(socket))
    }
    /// An Eaglercraft server at a `ws:`/`wss:` URL ([`crate::eagler`]); only
    /// the browser build has the WebSocket for it.
    pub fn connect_eagler(url: &str, timeout: Duration) -> Result<Self> {
        #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
        {
            let socket = horizon_os::net::WebSocket::connect_url(url, timeout)
                .map_err(|e| Error::new(format!("Connection refused: {e}")))?;
            Self::eagler(Box::new(socket))
        }
        #[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
        {
            let _ = timeout;
            Err(Error::new(format!(
                "{url} is an Eaglercraft server: join it from the browser build"
            )))
        }
    }
    /// A shared world's host, through a relay that knows its join code
    /// (horizon-os net.rs); only the browser build has WebRTC.
    pub fn connect_shared(code: &str, relays: &[String], timeout: Duration) -> Result<Self> {
        #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
        {
            let socket = horizon_os::net::WebSocket::connect_shared(code, relays, timeout)
                .map_err(|e| Error::new(e.to_string()))?;
            Self::over(Stream::Web(socket))
        }
        #[cfg(not(all(target_arch = "wasm32", target_feature = "atomics")))]
        {
            let _ = (code, relays, timeout);
            Err(Error::new(
                "Shared worlds are joined from the browser build",
            ))
        }
    }
    /// A player who joined a world this page shares, over their data channel.
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    pub fn from_web(socket: horizon_os::net::WebSocket) -> Result<Self> {
        Self::over(Stream::Web(socket))
    }
    /// A player who joined a world this page shares, opening with the
    /// Eaglercraft plugin handshake instead of the game's.
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    pub fn eagler_server_web(
        socket: horizon_os::net::WebSocket,
        first: crate::eagler::Message,
        brand: &str,
        version: &str,
    ) -> Result<Self> {
        Self::over(Stream::EaglerServer(
            crate::eagler_server::EaglerServerSocket::new(
                Box::new(WebMessages::with_first(socket, first)),
                brand,
                version,
            ),
        ))
    }
    /// A connection through the Eaglercraft plugin over a message socket.
    pub fn eagler(socket: Box<dyn crate::eagler::Messages>) -> Result<Self> {
        Self::over(Stream::Eagler(crate::eagler::EaglerSocket::new(socket)))
    }
    /// A shared world's end of an Eaglercraft connection: the guest's plugin
    /// handshake, then the game protocol as messages (`EaglerServerSocket`).
    pub fn eagler_server(
        socket: Box<dyn crate::eagler::Messages>,
        brand: &str,
        version: &str,
    ) -> Result<Self> {
        Self::over(Stream::EaglerServer(
            crate::eagler_server::EaglerServerSocket::new(socket, brand, version),
        ))
    }
    /// A shared world's end of an Eaglercraft connection for a 1.8 guest.
    pub fn eagler_server_legacy(
        socket: Box<dyn crate::eagler::Messages>,
        brand: &str,
        version: &str,
        resolve: std::sync::Arc<dyn Fn(u32) -> crate::translate::legacy::LegacyState + Send + Sync>,
    ) -> Result<Self> {
        Self::over(Stream::EaglerServer(
            crate::eagler_server::EaglerServerSocket::legacy(socket, brand, version, resolve),
        ))
    }
    /// The level's shape, for translating chunks to a legacy guest.
    pub fn set_legacy_chunk_shape(&self, shape: crate::translate::legacy::play::ChunkShape) {
        if let Stream::EaglerServer(socket) = &self.writer.stream {
            socket.set_chunk_shape(shape);
        }
    }
    /// The world's biomes by network id, for translating chunks to a legacy
    /// guest.
    pub fn set_legacy_biomes(&self, biomes: std::sync::Arc<dyn Fn(u32) -> u8 + Send + Sync>) {
        if let Stream::EaglerServer(socket) = &self.writer.stream {
            socket.set_biomes(biomes);
        }
    }
    /// The world's entity types by network id, for translating spawn packets
    /// to a legacy guest.
    pub fn set_legacy_entities(
        &self,
        entities: std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>,
    ) {
        if let Stream::EaglerServer(socket) = &self.writer.stream {
            socket.set_entities(entities);
        }
    }
    /// The world's item types by network id, for translating stacks to a
    /// legacy guest.
    pub fn set_legacy_items(
        &self,
        items: std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>,
    ) {
        if let Stream::EaglerServer(socket) = &self.writer.stream {
            socket.set_items(items);
        }
    }
    /// The same, over a shared world's data channel (the browser build).
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    pub fn eagler_server_web_legacy(
        socket: horizon_os::net::WebSocket,
        first: crate::eagler::Message,
        brand: &str,
        version: &str,
        resolve: std::sync::Arc<dyn Fn(u32) -> crate::translate::legacy::LegacyState + Send + Sync>,
    ) -> Result<Self> {
        Self::over(Stream::EaglerServer(
            crate::eagler_server::EaglerServerSocket::legacy(
                Box::new(WebMessages::with_first(socket, first)),
                brand,
                version,
                resolve,
            ),
        ))
    }
    /// A browser's connection, whose HTTP upgrade request is still to read
    /// ([`crate::websocket::is_http`] told it apart).
    pub fn from_websocket(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true)?;
        Self::over(Stream::WebSocket(crate::websocket::ServerSocket::accept(
            stream,
        )?))
    }
    /// A browser's connection whose upgrade is done and whose first message
    /// was read, to tell an Eaglercraft guest from a vanilla client.
    pub fn from_websocket_prefetched(
        socket: crate::websocket::ServerSocket,
        first: Vec<u8>,
    ) -> Result<Self> {
        Self::over(Stream::WebSocketPrefixed(crate::websocket::Prefixed::new(
            socket, first,
        )))
    }
    pub fn from_stream(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true)?;
        Self::over(Stream::Tcp(stream))
    }
    /// A connection over one end of an in-memory [`Pipe`].
    pub fn local(pipe: Pipe) -> Self {
        Self::over(Stream::Memory(pipe)).expect("a pipe end clones")
    }
    fn over(stream: Stream) -> Result<Self> {
        let counters = Arc::new(Counters::default());
        let write = stream.try_clone()?;
        Ok(Self {
            reader: ConnReader {
                stream,
                buffer: Vec::new(),
                decrypt: None,
                threshold: None,
                counters: counters.clone(),
            },
            writer: ConnWriter {
                stream: write,
                encrypt: None,
                threshold: None,
                counters,
            },
        })
    }
    /// `Connection.isMemoryConnection`: through a [`Pipe`] rather than a socket.
    pub fn is_memory(&self) -> bool {
        matches!(self.reader.stream, Stream::Memory(_))
    }
    /// ClientboundLoginCompressionPacket: negative thresholds disable compression.
    pub fn set_compression(&mut self, threshold: i32) {
        let t = usize::try_from(threshold).ok();
        self.reader.threshold = t;
        self.writer.threshold = t;
    }
    /// Connection.setEncryptionKey: both directions keyed with the shared secret.
    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        let mut decrypt = Cfb8::new(secret, secret);
        // Bytes already read past the last packet were sent encrypted.
        decrypt.decrypt(&mut self.reader.buffer);
        self.reader.decrypt = Some(decrypt);
        self.writer.encrypt = Some(Cfb8::new(secret, secret));
    }
    pub fn send(&mut self, body: &[u8]) -> Result<()> {
        self.writer.send(body)
    }
    pub fn recv(&mut self) -> Result<(i32, Vec<u8>)> {
        self.reader.recv()
    }
    pub fn split(self) -> (ConnReader, ConnWriter) {
        (self.reader, self.writer)
    }
}
impl ConnReader {
    /// Read exactly `len` bytes as they are, with no framing.
    pub fn read_raw(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        self.stream.read_exact(&mut out)?;
        Ok(out)
    }
    /// Block until a whole packet arrives: (id, payload).
    pub fn recv(&mut self) -> Result<(i32, Vec<u8>)> {
        loop {
            if let Some(frame) = frame::split(&mut self.buffer)? {
                let body = frame::decompress(frame, self.threshold)?;
                if body.is_empty() {
                    // A frame with no packet in it. Vanilla's PacketDecoder
                    // only decodes a packet from a readable buffer, so an
                    // empty compressed frame is skipped and the connection
                    // stays up.
                    continue;
                }
                let mut r = Reader::new(&body);
                let id = r.varint()?;
                let start = r.position();
                self.counters.received.fetch_add(1, Ordering::Relaxed);
                return Ok((id, body[start..].to_vec()));
            }
            let mut chunk = [0u8; 16384];
            let n = match self.stream.read(&mut chunk) {
                Ok(0) => return Err(Error::end_of_stream()),
                Ok(n) => n,
                // The read timeout passed: Windows reports TimedOut, Unix
                // WouldBlock.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(Error::timed_out());
                }
                Err(e) => return Err(e.into()),
            };
            self.counters
                .bytes_in
                .fetch_add(n as i64, Ordering::Relaxed);
            let data = &mut chunk[..n];
            if let Some(c) = &mut self.decrypt {
                c.decrypt(data);
            }
            self.buffer.extend_from_slice(data);
        }
    }
    /// A [`Closer`] for this connection.
    pub fn closer(&self) -> Result<Closer> {
        Ok(Closer(self.stream.try_clone()?))
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        Ok(self.stream.set_read_timeout(timeout)?)
    }
}
impl ConnWriter {
    /// Write bytes as they are, with no framing: the legacy server list ping
    /// (`LegacyServerPinger`) is not a framed protocol.
    pub fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data)?;
        self.stream.flush()?;
        Ok(())
    }
    /// Send one packet body (VarInt id + payload).
    pub fn send(&mut self, body: &[u8]) -> Result<()> {
        self.send_contents(&frame::contents(body, self.threshold)?)
    }
    /// Send a frame whose contents [`frame::contents`] made with this
    /// writer's threshold.
    pub fn send_contents(&mut self, contents: &[u8]) -> Result<()> {
        let mut framed = frame::prefixed(contents);
        if let Some(c) = &mut self.encrypt {
            c.encrypt(&mut framed);
        }
        self.stream.write_all(&framed)?;
        self.counters.sent.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_out
            .fetch_add(framed.len() as i64, Ordering::Relaxed);
        Ok(())
    }
    pub fn shutdown(&self) {
        self.stream.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_os::time::Instant;
    use std::net::TcpListener;

    #[test]
    fn ipv4_first_keeps_each_familys_order() {
        let a = |s: &str| s.parse::<SocketAddr>().unwrap();
        let resolved = vec![
            a("[2001:db8::1]:25565"),
            a("192.0.2.1:25565"),
            a("[2001:db8::2]:25565"),
            a("10.0.0.1:25565"),
        ];
        assert_eq!(
            ipv4_first(resolved),
            [
                a("192.0.2.1:25565"),
                a("10.0.0.1:25565"),
                a("[2001:db8::1]:25565"),
                a("[2001:db8::2]:25565"),
            ]
        );
    }

    fn peer(c: &Connection) -> SocketAddr {
        match &c.reader.stream {
            Stream::Tcp(s) => s.peer_addr().unwrap(),
            _ => panic!("not a TCP socket"),
        }
    }

    /// Binds `[::1]`, or `None` where the machine has no IPv6 loopback.
    fn ipv6_loopback() -> Option<TcpListener> {
        let listener = TcpListener::bind("[::1]:0");
        if listener.is_err() {
            eprintln!("skipped: no IPv6 loopback");
        }
        listener.ok()
    }

    #[test]
    fn connects_to_ipv4_although_the_resolver_lists_ipv6_first() {
        // Both listen, as with a server whose IPv6 address accepts and then resets.
        let Some(v6_listener) = ipv6_loopback() else {
            return;
        };
        let v4_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let v6 = v6_listener.local_addr().unwrap();
        let v4 = v4_listener.local_addr().unwrap();
        let c =
            Connection::connect_addresses("test", vec![v6, v4], Duration::from_secs(5)).unwrap();
        assert_eq!(peer(&c), v4);
    }

    #[test]
    fn falls_back_to_ipv6_when_ipv4_refuses() {
        let Some(v6_listener) = ipv6_loopback() else {
            return;
        };
        let v6 = v6_listener.local_addr().unwrap();
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let c = Connection::connect_addresses("test", vec![v6, closed], Duration::from_secs(5))
            .unwrap();
        assert_eq!(peer(&c), v6);
    }

    #[test]
    fn a_dead_address_does_not_hold_up_the_next() {
        // TEST-NET-1 never answers (or is unreachable at once).
        let dead: SocketAddr = "192.0.2.1:25565".parse().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live = listener.local_addr().unwrap();
        let start = Instant::now();
        let c = Connection::connect_addresses("test", vec![dead, live], Duration::from_secs(10))
            .unwrap();
        assert_eq!(peer(&c), live);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn every_failure_reports_the_first_address() {
        let closed = |_| {
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
        };
        let error = |host, addrs| {
            Connection::connect_addresses(host, addrs, Duration::from_secs(5))
                .err()
                .unwrap()
                .to_string()
        };
        let refused = error("test", (0..3).map(closed).collect());
        assert!(refused.starts_with("Connection refused"), "{refused}");
        assert_eq!(error("nowhere", Vec::new()), "Unknown host nowhere");
    }

    #[test]
    fn a_silent_peer_times_out_and_a_closed_one_ends_the_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut c = Connection::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap();
        let (server, _) = listener.accept().unwrap();
        c.reader
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let silent = c.recv().unwrap_err();
        assert!(silent.is_timed_out(), "{silent}");
        assert_eq!(silent.to_string(), "Timed out");
        drop(server);
        let closed = c.recv().unwrap_err();
        assert!(closed.is_end_of_stream(), "{closed}");
    }

    #[test]
    fn loopback_with_compression_and_encryption() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = horizon_os::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut c = Connection::from_stream(stream).unwrap();
            let (id, payload) = c.recv().unwrap();
            assert_eq!((id, payload.as_slice()), (0, &b"plain"[..]));
            c.enable_encryption(&[7; 16]);
            c.set_compression(64);
            let (id, payload) = c.recv().unwrap();
            assert_eq!((id, payload.len()), (5, 1000));
            let mut body = vec![9];
            body.extend_from_slice(b"reply");
            c.send(&body).unwrap();
        });
        let mut c = Connection::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap();
        let mut body = vec![0];
        body.extend_from_slice(b"plain");
        c.send(&body).unwrap();
        c.enable_encryption(&[7; 16]);
        c.set_compression(64);
        let mut big = vec![5];
        big.extend(std::iter::repeat_n(3u8, 1000));
        c.send(&big).unwrap();
        let (id, payload) = c.recv().unwrap();
        assert_eq!((id, payload.as_slice()), (9, &b"reply"[..]));
        server.join().unwrap();
    }

    #[test]
    fn local_pipe_carries_packets_and_closes() {
        let (a, b) = Pipe::pair();
        let server = horizon_os::thread::spawn(move || {
            let mut c = Connection::local(b);
            c.set_compression(64);
            let (id, payload) = c.recv().unwrap();
            assert_eq!((id, payload.len()), (5, 1000));
            c.send(&[9, 1, 2]).unwrap();
            let (_, writer) = c.split();
            writer.shutdown();
        });
        let mut c = Connection::local(a);
        c.set_compression(64);
        let mut big = vec![5];
        big.extend(std::iter::repeat_n(3u8, 1000));
        c.send(&big).unwrap();
        assert_eq!(c.recv().unwrap(), (9, vec![1, 2]));
        assert_eq!(c.reader.counters.sent.load(Ordering::Relaxed), 1);
        assert_eq!(c.reader.counters.received.load(Ordering::Relaxed), 1);
        server.join().unwrap();
        assert!(c.recv().is_err());
    }

    #[test]
    fn dropping_a_pipe_end_closes_it() {
        let (a, b) = Pipe::pair();
        let mut c = Connection::local(a);
        drop(b);
        assert!(c.recv().is_err());
        assert!(c.send(&[1]).is_err());
    }

    #[test]
    fn local_pipe_read_times_out() {
        let (a, _b) = Pipe::pair();
        let mut c = Connection::local(a);
        c.reader
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        assert!(c.recv().is_err());
    }

    /// Encrypted bytes that arrive in the same read as the last plain packet
    /// are decrypted once encryption is enabled.
    #[test]
    fn encrypted_bytes_read_before_enabling_encryption() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, both_sent) = std::sync::mpsc::channel();
        let client = horizon_os::thread::spawn(move || {
            let mut c = Connection::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap();
            c.send(&[0, 1, 2]).unwrap();
            c.enable_encryption(&[7; 16]);
            c.send(&[5, 4, 3]).unwrap();
            sent.send(c).unwrap();
        });
        let (stream, _) = listener.accept().unwrap();
        let mut c = Connection::from_stream(stream).unwrap();
        let _keep_open = both_sent.recv().unwrap();
        assert_eq!(c.recv().unwrap(), (0, vec![1, 2]));
        c.enable_encryption(&[7; 16]);
        assert_eq!(c.recv().unwrap(), (5, vec![4, 3]));
        client.join().unwrap();
    }

    /// A server may send a compressed frame that carries no packet: the
    /// uncompressed length is 0 and the frame holds nothing else. Vanilla
    /// skips it (its PacketDecoder only reads a packet from a readable
    /// buffer), so the client stays connected.
    #[test]
    fn an_empty_compressed_frame_is_skipped() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = horizon_os::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut c = Connection::from_stream(stream).unwrap();
            c.set_compression(64);
            // The empty frame: its length prefix, then the uncompressed
            // length 0 and nothing else.
            c.writer.send_contents(&[0]).unwrap();
            let mut body = vec![3];
            body.extend_from_slice(b"after");
            c.writer.send(&body).unwrap();
        });
        let mut c = Connection::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap();
        c.set_compression(64);
        let (id, payload) = c.recv().unwrap();
        assert_eq!((id, payload.as_slice()), (3, &b"after"[..]));
        server.join().unwrap();
    }
}

/// The browser's socket as the Eaglercraft plugin's message socket.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
impl crate::eagler::Messages for horizon_os::net::WebSocket {
    fn send(&self, message: crate::eagler::Message) -> std::io::Result<()> {
        use horizon_os::net::Message;
        horizon_os::net::WebSocket::send(
            self,
            match message {
                crate::eagler::Message::Binary(b) => Message::Binary(b),
                crate::eagler::Message::Text(t) => Message::Text(t),
            },
        )
    }
    fn recv(&self) -> std::io::Result<Option<crate::eagler::Message>> {
        use horizon_os::net::Message;
        Ok(horizon_os::net::WebSocket::recv(self)?.map(|m| match m {
            Message::Binary(b) => crate::eagler::Message::Binary(b),
            Message::Text(t) => crate::eagler::Message::Text(t),
        }))
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) {
        horizon_os::net::WebSocket::set_read_timeout(self, timeout);
    }
    fn shutdown(&self) {
        horizon_os::net::WebSocket::shutdown(self);
    }
    fn try_clone(&self) -> std::io::Result<Box<dyn crate::eagler::Messages>> {
        Ok(Box::new(horizon_os::net::WebSocket::try_clone(self)))
    }
}
