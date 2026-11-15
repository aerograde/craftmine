//! The smallest useful MQTT 3.1.1 client: CONNECT with a retained will,
//! SUBSCRIBE at QoS 0, PUBLISH at QoS 0, and PINGREQ. No tokio, no C.
use horizon_os::time::Duration;
use std::{
    io::{self, Read, Write},
    net::TcpStream,
};

/// The largest packet accepted from a broker.
pub const MAX_PACKET: usize = 1 << 20;

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Connect {
        client_id: String,
        keep_alive: u16,
        clean: bool,
        will: Option<(String, Vec<u8>)>,
        will_retain: bool,
    },
    ConnAck {
        code: u8,
    },
    Publish {
        topic: String,
        payload: Vec<u8>,
        retain: bool,
    },
    Subscribe {
        id: u16,
        filters: Vec<String>,
    },
    SubAck {
        id: u16,
        codes: Vec<u8>,
    },
    PingReq,
    PingResp,
    Disconnect,
    /// A packet this client does not need; the type byte, body skipped.
    Other(u8),
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let header = match self {
            Self::Connect {
                client_id,
                keep_alive,
                clean,
                will,
                will_retain,
            } => {
                put_string(&mut body, "MQTT");
                body.push(4); // 3.1.1
                let mut flags = 0u8;
                if *clean {
                    flags |= 0b0000_0010;
                }
                if will.is_some() {
                    flags |= 0b0000_0100;
                    if *will_retain {
                        flags |= 0b0010_0000;
                    }
                }
                body.push(flags);
                body.extend(keep_alive.to_be_bytes());
                put_string(&mut body, client_id);
                if let Some((topic, message)) = will {
                    put_string(&mut body, topic);
                    put_bytes(&mut body, message);
                }
                0x10
            }
            Self::ConnAck { code } => {
                body.push(0); // session present
                body.push(*code);
                0x20
            }
            Self::Publish {
                topic,
                payload,
                retain,
            } => {
                put_string(&mut body, topic);
                body.extend_from_slice(payload);
                0x30 | u8::from(*retain)
            }
            Self::Subscribe { id, filters } => {
                body.extend(id.to_be_bytes());
                for filter in filters {
                    put_string(&mut body, filter);
                    body.push(0); // QoS 0
                }
                0x82
            }
            Self::SubAck { id, codes } => {
                body.extend(id.to_be_bytes());
                body.extend_from_slice(codes);
                0x90
            }
            Self::PingReq => 0xc0,
            Self::PingResp => 0xd0,
            Self::Disconnect => 0xe0,
            Self::Other(kind) => (*kind) << 4,
        };
        let mut out = Vec::with_capacity(body.len() + 5);
        out.push(header);
        put_remaining(&mut out, body.len());
        out.extend(body);
        out
    }

    pub fn write_to(&self, writer: &mut dyn Write) -> io::Result<()> {
        writer.write_all(&self.encode())?;
        writer.flush()
    }

    pub fn read_from(reader: &mut dyn Read) -> io::Result<Self> {
        let mut first = [0u8; 1];
        reader.read_exact(&mut first)?;
        let remaining = read_remaining(reader)?;
        if remaining > MAX_PACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MQTT packet too large",
            ));
        }
        let mut body = vec![0u8; remaining];
        reader.read_exact(&mut body)?;
        parse(first[0], &body)
    }
}

fn parse(header: u8, body: &[u8]) -> io::Result<Packet> {
    let kind = header >> 4;
    let flags = header & 0x0f;
    let mut at = 0;
    match kind {
        1 => {
            let protocol = take_string(body, &mut at)?;
            let level = *body.get(at).ok_or_else(short)?;
            at += 1;
            let flags = *body.get(at).ok_or_else(short)?;
            at += 1;
            let keep_alive = take_u16(body, &mut at)?;
            let client_id = take_string(body, &mut at)?;
            let mut will = None;
            let will_retain = flags & 0b0010_0000 != 0;
            if flags & 0b0000_0100 != 0 {
                let topic = take_string(body, &mut at)?;
                let message = take_bytes(body, &mut at)?;
                will = Some((topic, message));
            }
            if flags & 0b1000_0000 != 0 {
                let _ = take_string(body, &mut at)?;
            }
            if flags & 0b0100_0000 != 0 {
                let _ = take_bytes(body, &mut at)?;
            }
            if protocol != "MQTT" || level != 4 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "not MQTT 3.1.1"));
            }
            Ok(Packet::Connect {
                client_id,
                keep_alive,
                clean: flags & 0b0000_0010 != 0,
                will,
                will_retain,
            })
        }
        2 => {
            let code = *body.get(1).ok_or_else(short)?;
            Ok(Packet::ConnAck { code })
        }
        3 => {
            let topic = take_string(body, &mut at)?;
            let qos = (flags >> 1) & 0b11;
            if qos > 0 {
                let _ = take_u16(body, &mut at)?;
            }
            Ok(Packet::Publish {
                topic,
                payload: body.get(at..).unwrap_or_default().to_vec(),
                retain: flags & 1 != 0,
            })
        }
        8 => {
            let id = take_u16(body, &mut at)?;
            let mut filters = Vec::new();
            while at < body.len() {
                filters.push(take_string(body, &mut at)?);
                at += 1; // requested QoS
            }
            Ok(Packet::Subscribe { id, filters })
        }
        9 => {
            let id = take_u16(body, &mut at)?;
            Ok(Packet::SubAck {
                id,
                codes: body.get(at..).unwrap_or_default().to_vec(),
            })
        }
        12 => Ok(Packet::PingReq),
        13 => Ok(Packet::PingResp),
        14 => Ok(Packet::Disconnect),
        other => Ok(Packet::Other(other)),
    }
}

fn short() -> io::Error {
    io::ErrorKind::UnexpectedEof.into()
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_bytes(out, value.as_bytes());
}
fn put_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend((value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}
fn take_u16(body: &[u8], at: &mut usize) -> io::Result<u16> {
    let bytes = body.get(*at..*at + 2).ok_or_else(short)?;
    *at += 2;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn take_string(body: &[u8], at: &mut usize) -> io::Result<String> {
    String::from_utf8(take_bytes(body, at)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}
fn take_bytes(body: &[u8], at: &mut usize) -> io::Result<Vec<u8>> {
    let len = usize::from(take_u16(body, at)?);
    let bytes = body.get(*at..*at + len).ok_or_else(short)?;
    *at += len;
    Ok(bytes.to_vec())
}

pub fn put_remaining(out: &mut Vec<u8>, mut len: usize) {
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

pub fn read_remaining(reader: &mut dyn Read) -> io::Result<usize> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for _ in 0..4 {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        value += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            return Ok(value);
        }
        multiplier *= 128;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "bad MQTT remaining length",
    ))
}

/// A broker connection: TCP, TLS or the browser's WebSocket.
pub trait Stream: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn shutdown(&self);
}

impl Stream for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn shutdown(&self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Mqtt,
    Mqtts,
    Ws,
    Wss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerUrl {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl BrokerUrl {
    pub fn parse(url: &str) -> io::Result<Self> {
        let (scheme, rest) = if let Some(rest) = url.strip_prefix("mqtts://") {
            (Scheme::Mqtts, rest)
        } else if let Some(rest) = url.strip_prefix("mqtt://") {
            (Scheme::Mqtt, rest)
        } else if let Some(rest) = url.strip_prefix("wss://") {
            (Scheme::Wss, rest)
        } else if let Some(rest) = url.strip_prefix("ws://") {
            (Scheme::Ws, rest)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not an MQTT broker URL: {url}"),
            ));
        };
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, String::new()),
        };
        let (host, port) = if let Some(after) = authority.strip_prefix('[') {
            let end = after
                .find(']')
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad IPv6 broker"))?;
            let (host, tail) = after.split_at(end);
            let port = match tail.strip_prefix("]:") {
                Some(port) => Some(parse_port(port)?),
                None if tail == "]" => None,
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "bad broker address",
                    ));
                }
            };
            (host.to_owned(), port)
        } else if let Some((host, port)) = authority.rsplit_once(':') {
            (host.to_owned(), Some(parse_port(port)?))
        } else {
            (authority.to_owned(), None)
        };
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the broker has no host",
            ));
        }
        Ok(Self {
            scheme,
            host,
            port: port.unwrap_or(match scheme {
                Scheme::Mqtt => 1883,
                Scheme::Mqtts => 8883,
                Scheme::Ws => 80,
                Scheme::Wss => 443,
            }),
            path,
        })
    }
}

fn parse_port(text: &str) -> io::Result<u16> {
    text.parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad broker port"))
}

/// Open a broker connection.
pub fn connect(url: &str, timeout: Duration) -> io::Result<Box<dyn Stream>> {
    let url = BrokerUrl::parse(url)?;
    connect_parsed(&url, timeout)
}

#[cfg(not(target_arch = "wasm32"))]
fn connect_parsed(url: &BrokerUrl, timeout: Duration) -> io::Result<Box<dyn Stream>> {
    use std::net::ToSocketAddrs;
    let target = (url.host.as_str(), url.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the broker did not resolve"))?;
    let tcp = TcpStream::connect_timeout(&target, timeout)?;
    let _ = tcp.set_nodelay(true);
    match url.scheme {
        Scheme::Mqtt => Ok(Box::new(tcp)),
        Scheme::Mqtts => tls(url, tcp),
        Scheme::Ws | Scheme::Wss => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MQTT over WebSockets is for the browser build",
        )),
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn connect_parsed(url: &BrokerUrl, timeout: Duration) -> io::Result<Box<dyn Stream>> {
    match url.scheme {
        Scheme::Ws | Scheme::Wss => {
            let ws = horizon_os::net::WebSocket::connect_url_with(
                &format!(
                    "{}://{}:{}{}",
                    if url.scheme == Scheme::Wss {
                        "wss"
                    } else {
                        "ws"
                    },
                    url.host,
                    url.port,
                    url.path
                ),
                "mqtt",
                timeout,
            )?;
            Ok(Box::new(ws))
        }
        Scheme::Mqtt | Scheme::Mqtts => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the browser reaches MQTT over WebSockets",
        )),
    }
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn connect_parsed(_url: &BrokerUrl, _timeout: Duration) -> io::Result<Box<dyn Stream>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the browser build without threads has no networking",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
struct Tls(rustls::StreamOwned<rustls::ClientConnection, TcpStream>);
#[cfg(not(target_arch = "wasm32"))]
impl Read for Tls {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Write for Tls {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Stream for Tls {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.sock.set_read_timeout(timeout)
    }
    fn shutdown(&self) {
        let _ = self.0.sock.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn tls(url: &BrokerUrl, tcp: TcpStream) -> io::Result<Box<dyn Stream>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(url.host.clone())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), name)
        .map_err(io::Error::other)?;
    Ok(Box::new(Tls(rustls::StreamOwned::new(connection, tcp))))
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
impl Stream for horizon_os::net::WebSocket {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        horizon_os::net::WebSocket::set_read_timeout(self, timeout);
        Ok(())
    }
    fn shutdown(&self) {
        horizon_os::net::WebSocket::shutdown(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_round_trips() {
        let packet = Packet::Connect {
            client_id: "hzn123".into(),
            keep_alive: 30,
            clean: true,
            will: Some(("horizon/nearby/v1/room/presence/id".into(), Vec::new())),
            will_retain: true,
        };
        let bytes = packet.encode();
        assert_eq!(bytes[0], 0x10);
        let mut cursor = &bytes[..];
        let parsed = Packet::read_from(&mut cursor).unwrap();
        assert_eq!(parsed, packet);
    }

    #[test]
    fn publish_and_subscribe_round_trip() {
        let publish = Packet::Publish {
            topic: "horizon/nearby/v1/room/presence/a".into(),
            payload: b"hello".to_vec(),
            retain: true,
        };
        let mut bytes = &publish.encode()[..];
        assert_eq!(Packet::read_from(&mut bytes).unwrap(), publish);
        let subscribe = Packet::Subscribe {
            id: 3,
            filters: vec![
                "horizon/nearby/v1/room/presence/+".into(),
                "horizon/nearby/v1/room/inbox/a".into(),
            ],
        };
        let mut bytes = &subscribe.encode()[..];
        assert_eq!(Packet::read_from(&mut bytes).unwrap(), subscribe);
    }

    #[test]
    fn remaining_lengths_use_seven_bits() {
        let mut out = Vec::new();
        put_remaining(&mut out, 0);
        assert_eq!(out, [0]);
        out.clear();
        put_remaining(&mut out, 127);
        assert_eq!(out, [127]);
        out.clear();
        put_remaining(&mut out, 128);
        assert_eq!(out, [0x80, 0x01]);
        out.clear();
        put_remaining(&mut out, 16_383);
        assert_eq!(out, [0xff, 0x7f]);
        out.clear();
        put_remaining(&mut out, 16_384);
        assert_eq!(out, [0x80, 0x80, 0x01]);
        let mut cursor = &out[..];
        assert_eq!(read_remaining(&mut cursor).unwrap(), 16_384);
    }

    #[test]
    fn broker_urls_are_parsed() {
        let u = BrokerUrl::parse("mqtts://broker.emqx.io:8883").unwrap();
        assert_eq!(
            (u.scheme, u.host.as_str(), u.port),
            (Scheme::Mqtts, "broker.emqx.io", 8883)
        );
        let u = BrokerUrl::parse("wss://broker.emqx.io:8084/mqtt").unwrap();
        assert_eq!(
            (u.scheme, u.port, u.path.as_str()),
            (Scheme::Wss, 8084, "/mqtt")
        );
        let u = BrokerUrl::parse("mqtt://test.mosquitto.org").unwrap();
        assert_eq!(u.port, 1883);
        let u = BrokerUrl::parse("wss://[2001:db8::1]:443/mqtt").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("2001:db8::1", 443));
        assert!(BrokerUrl::parse("https://example.com").is_err());
    }
}
