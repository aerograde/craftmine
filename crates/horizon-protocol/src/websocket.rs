//! The server's end of a WebSocket (RFC 6455), which browsers reach a server
//! by: the page has no TCP. A connection on the server's port that starts
//! with an HTTP `GET` is upgraded, then carries the same bytes a TCP
//! connection does, in binary messages.
use sha1::{Digest, Sha1};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{Arc, Mutex},
};

/// Whether a connection's first bytes are an HTTP request rather than a
/// Minecraft frame (whose second byte, the packet id, is 0 in a handshake).
pub fn is_http(first: &[u8]) -> bool {
    first.starts_with(b"GET ")
}

/// The handshake's accept key: SHA-1 of the client's key and RFC 6455's GUID.
fn accept_key(key: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(key.as_bytes());
    sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64(&sha.finalize())
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The server's end of an upgraded connection.
pub struct ServerSocket {
    stream: TcpStream,
    /// Held for each whole frame written: the reader answers pings beside
    /// the writer.
    writing: Arc<Mutex<()>>,
    /// What is left of the data frame being read, and its mask.
    left: u64,
    mask: [u8; 4],
    offset: usize,
    /// Whether the data frame being read is text (a status query).
    text: bool,
}

impl ServerSocket {
    /// Reads the HTTP upgrade request and answers it. A request that isn't a
    /// WebSocket's (a browser opening the address as a page) gets a short
    /// refusal.
    pub fn accept(mut stream: TcpStream) -> io::Result<Self> {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
        let mut request = Vec::new();
        let mut byte = [0];
        // One byte at a time: nothing past the request may be taken.
        while !request.ends_with(b"\r\n\r\n") {
            if request.len() > 8192 || stream.read(&mut byte)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bad HTTP request",
                ));
            }
            request.push(byte[0]);
        }
        let request = String::from_utf8_lossy(&request);
        let key = request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("sec-websocket-key")
                .then(|| value.trim().to_owned())
        });
        let Some(key) = key else {
            let body = "This is a Minecraft server: join it from the game.\n";
            let _ = write!(
                stream,
                "HTTP/1.1 426 Upgrade Required\r\nUpgrade: websocket\r\nConnection: close\r\n\
                 Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a WebSocket request",
            ));
        };
        write!(
            stream,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(&key)
        )?;
        Ok(Self {
            stream,
            writing: Arc::default(),
            left: 0,
            mask: [0; 4],
            offset: 0,
            text: false,
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            writing: self.writing.clone(),
            left: 0,
            mask: [0; 4],
            offset: 0,
            text: false,
        })
    }

    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// The next whole data message (`None`: the client closed).
    pub fn recv_message(&mut self) -> io::Result<Option<crate::eagler::Message>> {
        if self.left == 0 && self.next_frame()?.is_none() {
            return Ok(None);
        }
        let len = usize::try_from(self.left).unwrap_or(usize::MAX);
        let mut data = vec![0; len];
        self.stream.read_exact(&mut data)?;
        for b in &mut data {
            *b ^= self.mask[self.offset % 4];
            self.offset += 1;
        }
        self.left = 0;
        self.offset = 0;
        Ok(Some(if self.text {
            crate::eagler::Message::Text(String::from_utf8_lossy(&data).into_owned())
        } else {
            crate::eagler::Message::Binary(data)
        }))
    }

    /// One whole message to the browser.
    pub fn send_message(&self, message: crate::eagler::Message) -> io::Result<()> {
        match message {
            crate::eagler::Message::Binary(data) => self.send_frame(2, &data),
            crate::eagler::Message::Text(text) => self.send_frame(1, text.as_bytes()),
        }
    }

    fn send_frame(&self, opcode: u8, data: &[u8]) -> io::Result<()> {
        let mut frame = vec![0x80 | opcode];
        match data.len() {
            n @ 0..126 => frame.push(n as u8),
            n @ 126..65536 => {
                frame.push(126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(data);
        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        (&self.stream).write_all(&frame)
    }

    /// The next frame's header. Data frames leave their payload to read;
    /// control frames are handled here. `None`: the client closed.
    fn next_frame(&mut self) -> io::Result<Option<()>> {
        loop {
            let mut head = [0; 2];
            self.stream.read_exact(&mut head)?;
            let opcode = head[0] & 0x0F;
            let mut len = u64::from(head[1] & 0x7F);
            if len == 126 {
                let mut n = [0; 2];
                self.stream.read_exact(&mut n)?;
                len = u16::from_be_bytes(n).into();
            } else if len == 127 {
                let mut n = [0; 8];
                self.stream.read_exact(&mut n)?;
                len = u64::from_be_bytes(n);
            }
            let mut mask = [0; 4];
            if head[1] & 0x80 != 0 {
                self.stream.read_exact(&mut mask)?;
            }
            match opcode {
                // Continuation, text, binary: all bytes of the stream.
                0..=2 => {
                    self.text = opcode == 1;
                    (self.left, self.mask, self.offset) = (len, mask, 0);
                    return Ok(Some(()));
                }
                8..=10 if len <= 125 => {
                    let mut payload = vec![0; len as usize];
                    self.stream.read_exact(&mut payload)?;
                    for (i, b) in payload.iter_mut().enumerate() {
                        *b ^= mask[i % 4];
                    }
                    match opcode {
                        8 => {
                            let _ = self.send_frame(8, &payload[..payload.len().min(2)]);
                            return Ok(None);
                        }
                        9 => self.send_frame(10, &payload)?,
                        _ => {}
                    }
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "bad WebSocket frame",
                    ));
                }
            }
        }
    }
}

impl Read for ServerSocket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.left == 0 {
            if self.next_frame()?.is_none() {
                return Ok(0);
            }
        }
        let want = out
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.stream.read(&mut out[..want])?;
        if n == 0 {
            return Ok(0);
        }
        for b in &mut out[..n] {
            *b ^= self.mask[self.offset % 4];
            self.offset += 1;
        }
        self.left -= n as u64;
        Ok(n)
    }
}

impl Write for ServerSocket {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.send_frame(2, data)?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A server WebSocket as an Eaglercraft [`Messages`](crate::eagler::Messages)
/// socket: whole messages in and out, as the plugin's protocol is.
pub struct ServerMessages {
    socket: Mutex<ServerSocket>,
    /// The message read to tell an Eaglercraft guest from a vanilla client.
    first: Mutex<Option<crate::eagler::Message>>,
}

impl ServerMessages {
    pub fn new(socket: ServerSocket) -> Self {
        Self {
            socket: Mutex::new(socket),
            first: Mutex::new(None),
        }
    }

    /// For the first message the caller already read.
    pub fn with_first(socket: ServerSocket, first: crate::eagler::Message) -> Self {
        Self {
            socket: Mutex::new(socket),
            first: Mutex::new(Some(first)),
        }
    }
}

impl crate::eagler::Messages for ServerMessages {
    fn send(&self, message: crate::eagler::Message) -> io::Result<()> {
        self.socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_message(message)
    }
    fn recv(&self) -> io::Result<Option<crate::eagler::Message>> {
        if let Some(first) = self.first.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Ok(Some(first));
        }
        self.socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv_message()
    }
    fn set_read_timeout(&self, timeout: Option<std::time::Duration>) {
        let _ = self
            .socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_read_timeout(timeout);
    }
    fn shutdown(&self) {
        self.socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown();
    }
    fn try_clone(&self) -> io::Result<Box<dyn crate::eagler::Messages>> {
        let socket = self
            .socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_clone()?;
        Ok(Box::new(ServerMessages::new(socket)))
    }
}

/// A server WebSocket whose first message has already been read: its bytes
/// are served as stream bytes first, then the socket's. A vanilla client's
/// first message is its handshake frame, read to tell the two apart.
pub struct Prefixed {
    socket: ServerSocket,
    first: Vec<u8>,
}

impl Prefixed {
    pub fn new(socket: ServerSocket, first: Vec<u8>) -> Self {
        Self { socket, first }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: self.socket.try_clone()?,
            first: self.first.clone(),
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    pub fn shutdown(&self) {
        self.socket.shutdown();
    }
}

impl Read for Prefixed {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.first.is_empty() {
            let n = self.first.len().min(out.len());
            out[..n].copy_from_slice(&self.first[..n]);
            self.first.drain(..n);
            return Ok(n);
        }
        self.socket.read(out)
    }
}

impl Write for Prefixed {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.socket.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc_6455() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn http_is_told_from_a_handshake() {
        assert!(is_http(b"GET / HTTP/1.1"));
        // A handshake frame 71 bytes long ('G'), packet id 0.
        assert!(!is_http(&[b'G', 0, 0xF7, 5]));
        assert!(!is_http(&[0xFE, 1]));
    }

    /// Whole messages keep their boundaries and their kind, as the
    /// Eaglercraft transport reads them.
    #[test]
    fn whole_messages_keep_their_boundaries() {
        use crate::eagler::Message;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = horizon_os::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = ServerSocket::accept(stream).unwrap();
            assert_eq!(
                socket.recv_message().unwrap(),
                Some(Message::Binary(vec![1, 2]))
            );
            assert_eq!(
                socket.recv_message().unwrap(),
                Some(Message::Text("hi".into()))
            );
            socket.send_message(Message::Binary(vec![9])).unwrap();
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(
            b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .unwrap();
        let mut reply = Vec::new();
        let mut byte = [0];
        while !reply.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            reply.push(byte[0]);
        }
        let masked = |opcode: u8, data: &[u8]| {
            let mask = [1, 2, 3, 4];
            let mut frame = vec![0x80 | opcode, 0x80 | data.len() as u8];
            frame.extend_from_slice(&mask);
            frame.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            frame
        };
        s.write_all(&masked(2, &[1, 2])).unwrap();
        s.write_all(&masked(1, b"hi")).unwrap();
        let mut head = [0; 2];
        s.read_exact(&mut head).unwrap();
        assert_eq!(head, [0x82, 1]);
        let mut body = [0; 1];
        s.read_exact(&mut body).unwrap();
        assert_eq!(body, [9]);
        server.join().unwrap();
    }

    /// A browser's side: the upgrade, then masked binary frames, split
    /// anywhere; a ping on the way is answered.
    #[test]
    fn carries_packets_after_the_upgrade() {
        use crate::connection::Connection;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = horizon_os::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut head = [0; 4];
            let n = stream.peek(&mut head).unwrap();
            assert!(is_http(&head[..n]));
            let mut c = Connection::from_websocket(stream).unwrap();
            c.set_compression(64);
            let (id, payload) = c.recv().unwrap();
            assert_eq!((id, payload.len()), (5, 1000));
            c.send(&[9, 1, 2]).unwrap();
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(
            b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .unwrap();
        let mut reply = Vec::new();
        let mut byte = [0];
        while !reply.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            reply.push(byte[0]);
        }
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 101"), "{reply}");
        assert!(reply.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{reply}");
        let masked = |opcode: u8, data: &[u8]| {
            let mask = [1, 2, 3, 4];
            let mut f = vec![0x80 | opcode];
            if data.len() < 126 {
                f.push(0x80 | data.len() as u8);
            } else {
                f.push(0x80 | 126);
                f.extend_from_slice(&(data.len() as u16).to_be_bytes());
            }
            f.extend_from_slice(&mask);
            f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            f
        };
        let mut body = vec![5];
        body.extend(std::iter::repeat_n(3u8, 1000));
        let packet = crate::frame::prefixed(&crate::frame::contents(&body, Some(64)).unwrap());
        let (a, b) = packet.split_at(7);
        s.write_all(&masked(2, a)).unwrap();
        s.write_all(&masked(9, b"hi")).unwrap();
        s.write_all(&masked(2, b)).unwrap();
        // The pong, then the reply in one binary frame.
        let mut pong = [0; 4];
        s.read_exact(&mut pong).unwrap();
        assert_eq!(pong, [0x8A, 2, b'h', b'i']);
        let mut head = [0; 2];
        s.read_exact(&mut head).unwrap();
        assert_eq!(head[0], 0x82);
        let mut frame = vec![0; head[1] as usize];
        s.read_exact(&mut frame).unwrap();
        // Length 4, uncompressed (0), id 9, payload [1, 2].
        assert_eq!(frame, [4, 0, 9, 1, 2]);
        server.join().unwrap();
    }
}
