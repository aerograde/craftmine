//! The room is what makes two nearby clients meet on a public broker.
//!
//! `auto-<hash of the public IP>` groups everyone behind one NAT; a player
//! on a network where STUN failed gets `lan-<hash of the private /24>`
//! instead, and a custom code overrides both (VPNs, different networks).
use crate::proto::hex;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};

/// The automatic room for a public address, or the LAN fallback.
pub fn automatic(public_ip: Option<IpAddr>) -> Option<String> {
    public_ip
        .and_then(wan)
        .or_else(|| lan(horizon_friends::reach::interface_addresses(false)))
}

/// `auto-` + 12 hex of SHA-256 over the public address.
pub fn wan(ip: IpAddr) -> Option<String> {
    let ip = match ip {
        IpAddr::V4(v4) if !v4.is_unspecified() && !v4.is_loopback() => ip,
        IpAddr::V6(v6) if !v6.is_unspecified() && !v6.is_loopback() => ip,
        _ => return None,
    };
    Some(format!("auto-{}", digest(&[ip.to_string().as_bytes()])))
}

/// `lan-` + 12 hex over the private /24s, sorted; `None` when there is none.
pub fn lan(addresses: Vec<IpAddr>) -> Option<String> {
    let mut nets: Vec<String> = addresses
        .into_iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) if is_private(v4) => Some(subnet(v4)),
            _ => None,
        })
        .collect();
    nets.sort();
    nets.dedup();
    if nets.is_empty() {
        return None;
    }
    let parts: Vec<&[u8]> = nets.iter().map(|s| s.as_bytes()).collect();
    Some(format!("lan-{}", digest(&parts)))
}

/// A user code: 4-16 ASCII letters/digits, lowercased. It is the room itself.
pub fn custom(code: &str) -> Option<String> {
    let code: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (4..=16).contains(&code.len()).then_some(code)
}

/// The room a client should join: the custom code, else the automatic one.
pub fn resolve(custom_code: Option<&str>, public_ip: Option<IpAddr>) -> Option<String> {
    custom_code
        .and_then(custom)
        .or_else(|| automatic(public_ip))
}

fn digest(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    h.update(b"horizon-nearby room v1");
    for part in parts {
        h.update((part.len() as u32).to_be_bytes());
        h.update(part);
    }
    hex(&h.finalize()[..6])
}

fn subnet(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    format!("{}.{}.{}.0/24", o[0], o[1], o[2])
}

fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_public_address_is_the_same_room() {
        let a = wan("203.0.113.7".parse().unwrap()).unwrap();
        let b = wan("203.0.113.7".parse().unwrap()).unwrap();
        let c = wan("203.0.113.8".parse().unwrap()).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("auto-"));
        assert_eq!(a.len(), "auto-".len() + 12);
        assert_eq!(wan("127.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn the_lan_fallback_uses_private_subnets() {
        let a = lan(vec![
            "192.168.1.20".parse().unwrap(),
            "192.168.1.30".parse().unwrap(),
            "10.0.0.4".parse().unwrap(),
        ])
        .unwrap();
        let b = lan(vec![
            "10.0.0.4".parse().unwrap(),
            "192.168.1.99".parse().unwrap(),
        ])
        .unwrap();
        assert_eq!(a, b, "order and host bits do not matter");
        assert!(a.starts_with("lan-"));
        assert_eq!(lan(vec!["8.8.8.8".parse().unwrap()]), None);
    }

    #[test]
    fn custom_codes_are_normalized() {
        assert_eq!(custom(" A1-b2 ").as_deref(), Some("a1b2"));
        assert_eq!(custom("abc"), None);
        assert_eq!(custom("a".repeat(17).as_str()), None);
        assert_eq!(
            resolve(Some("Room-7"), Some("1.2.3.4".parse().unwrap())).as_deref(),
            Some("room7")
        );
    }
}
