//! SDP arrives from a browser with `a=candidate` lines whose address is an
//! mDNS name. Rewrite those to resolved IPs before `str0m` parses the SDP.
use horizon_os::time::Duration;

/// Rewrite `.local` ICE candidates in an SDP JSON payload (`{type, sdp}`).
/// `None` when the payload is not JSON or nothing had to change.
pub(crate) fn rewrite_mdns(json: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    let sdp = value.get("sdp")?.as_str()?.to_owned();
    let mut changed = false;
    let mut out = String::with_capacity(sdp.len());
    for line in sdp.lines() {
        let name = if line.starts_with("a=candidate:") {
            line.split_whitespace()
                .nth(4)
                .filter(|a| a.ends_with(".local"))
        } else {
            None
        };
        let Some(name) = name else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        changed = true;
        let ips = crate::mdns::resolve(name, Duration::from_millis(1500));
        if ips.is_empty() {
            log::warn!("horizon-webrtc: could not resolve ICE candidate {name}");
            continue;
        }
        for ip in ips {
            out.push_str(&line.replacen(name, &ip.to_string(), 1));
            out.push('\n');
        }
    }
    if !changed {
        return None;
    }
    value["sdp"] = serde_json::Value::String(out);
    serde_json::to_string(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_sdp_is_left_alone() {
        assert_eq!(rewrite_mdns("not json"), None);
        assert_eq!(rewrite_mdns(r#"{"type":"offer","sdp":"v=0\r\n"}"#), None);
    }
}
