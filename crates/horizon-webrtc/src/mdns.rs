//! Browser ICE candidates name their addresses with mDNS (`<id>.local`).
//! `str0m` takes socket addresses, so a native peer resolves those names
//! before it adds the candidate.
use horizon_os::time::Duration;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    time::Instant,
};

/// The addresses `name` resolves to, A and AAAA, within `timeout`. The
/// system's mDNS responder is asked first; a unicast mDNS query (QU bit) is
/// the fallback, for systems without one.
pub(crate) fn resolve(name: &str, timeout: Duration) -> Vec<IpAddr> {
    if !name.ends_with(".local") {
        return Vec::new();
    }
    if let Ok(addrs) = (name, 0).to_socket_addrs() {
        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        if !ips.is_empty() {
            return ips;
        }
    }
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return Vec::new();
    };
    let _ = socket.set_multicast_ttl_v4(255);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
    let target: SocketAddr = "224.0.0.251:5353".parse().expect("a literal address");
    let a = query(name, 1);
    let aaaa = query(name, 28);
    let _ = socket.send_to(&a, target);
    let _ = socket.send_to(&aaaa, target);
    let start = Instant::now();
    let deadline = start + timeout;
    let mut ips = Vec::new();
    let mut buf = [0u8; 1500];
    let mut resent = false;
    while Instant::now() < deadline {
        if !resent && start.elapsed() > timeout / 2 {
            let _ = socket.send_to(&a, target);
            let _ = socket.send_to(&aaaa, target);
            resent = true;
        }
        let Ok((n, _)) = socket.recv_from(&mut buf) else {
            continue;
        };
        parse_answers(&buf[..n], &mut ips);
        dedupe(&mut ips);
        if !ips.is_empty() {
            break;
        }
    }
    dedupe(&mut ips);
    ips
}

fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 20);
    out.extend(0u16.to_be_bytes()); // id
    out.extend(0u16.to_be_bytes()); // flags: standard query
    out.extend(1u16.to_be_bytes()); // one question
    out.extend(0u16.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len().min(63) as u8);
        out.extend(label.as_bytes());
    }
    out.push(0);
    out.extend(qtype.to_be_bytes());
    // IN, with the unicast-response bit set.
    out.extend(0x8001u16.to_be_bytes());
    out
}

fn parse_answers(buf: &[u8], out: &mut Vec<IpAddr>) {
    if buf.len() < 12 {
        return;
    }
    let questions = usize::from(u16::from_be_bytes([buf[4], buf[5]]));
    let answers = usize::from(u16::from_be_bytes([buf[6], buf[7]]));
    let mut pos = 12;
    for _ in 0..questions {
        if read_name(buf, &mut pos).is_none() {
            return;
        }
        pos += 4;
    }
    for _ in 0..answers {
        if read_name(buf, &mut pos).is_none() {
            return;
        }
        let Some(head) = buf.get(pos..pos + 10) else {
            return;
        };
        let kind = u16::from_be_bytes([head[0], head[1]]);
        let class = u16::from_be_bytes([head[2], head[3]]) & 0x7fff;
        let len = usize::from(u16::from_be_bytes([head[8], head[9]]));
        pos += 10;
        let Some(data) = buf.get(pos..pos + len) else {
            return;
        };
        if class == 1 {
            match kind {
                1 if len == 4 => out.push(IpAddr::V4(Ipv4Addr::new(
                    data[0], data[1], data[2], data[3],
                ))),
                28 if len == 16 => {
                    let mut o = [0u8; 16];
                    o.copy_from_slice(data);
                    out.push(IpAddr::V6(Ipv6Addr::from(o)));
                }
                _ => {}
            }
        }
        pos += len;
    }
}

/// A DNS name, following compression pointers. `pos` moves past the name.
fn read_name(buf: &[u8], pos: &mut usize) -> Option<String> {
    let mut name = String::new();
    let mut at = *pos;
    let mut jumped = false;
    let mut steps = 0;
    loop {
        steps += 1;
        if steps > 128 {
            return None;
        }
        let len = *buf.get(at)?;
        if len == 0 {
            if !jumped {
                *pos = at + 1;
            }
            return Some(name);
        }
        if len & 0xc0 == 0xc0 {
            let low = *buf.get(at + 1)?;
            let offset = (usize::from(len & 0x3f) << 8) | usize::from(low);
            if !jumped {
                *pos = at + 2;
                jumped = true;
            }
            at = offset;
            continue;
        }
        let end = at + 1 + usize::from(len);
        let label = std::str::from_utf8(buf.get(at + 1..end)?).ok()?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(label);
        at = end;
        if !jumped {
            *pos = at;
        }
    }
}

fn dedupe(ips: &mut Vec<IpAddr>) {
    ips.sort();
    ips.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_with_compression() {
        // "host.local" at 0, then a pointer to it from 12.
        let mut msg = Vec::new();
        msg.extend([
            4, b'h', b'o', b's', b't', 5, b'l', b'o', b'c', b'a', b'l', 0,
        ]);
        let mut pos = 12;
        msg.extend([0xc0, 0x00]);
        assert_eq!(read_name(&msg, &mut pos).as_deref(), Some("host.local"));
        assert_eq!(pos, 14);
    }

    #[test]
    fn a_records_are_read() {
        let mut msg = vec![0, 0, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        msg.extend([
            4, b'h', b'o', b's', b't', 5, b'l', b'o', b'c', b'a', b'l', 0,
        ]);
        msg.extend([0, 1, 0x80, 1]); // A, IN, unicast
        msg.extend([
            4, b'h', b'o', b's', b't', 5, b'l', b'o', b'c', b'a', b'l', 0,
        ]);
        msg.extend([0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 7]);
        let mut ips = Vec::new();
        parse_answers(&msg, &mut ips);
        assert_eq!(ips, [IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))]);
    }
}
