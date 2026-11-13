//! Bridge a plain stream (the game's memory pipe) and the friends tunnel's
//! secure records. `horizon-friends::pump` does this for TCP; this one is
//! generic so the same code carries WebRTC data channels.
use horizon_friends::crypto::{Frame, MAX_PAYLOAD, SecureReader, SecureWriter};
use horizon_os::time::Duration;
use std::io::{self, Read, Write};

/// The plaintext side: the game's end of a memory pipe (or a socket).
pub trait Plain: Read + Write + Send + 'static {
    fn try_clone_plain(&self) -> io::Result<Self>
    where
        Self: Sized;
    fn shutdown_plain(&self);
}

/// The secure side's carrier: a data-channel handle.
pub trait Secure: Read + Write + Send + 'static {
    fn set_secure_timeout(&self, timeout: Option<Duration>);
    fn shutdown_secure(&self);
}

impl Plain for horizon_protocol::connection::Pipe {
    fn try_clone_plain(&self) -> io::Result<Self> {
        Ok(self.try_clone())
    }
    fn shutdown_plain(&self) {
        self.shutdown();
    }
}
impl Secure for horizon_protocol::connection::Pipe {
    fn set_secure_timeout(&self, _timeout: Option<Duration>) {}
    fn shutdown_secure(&self) {
        self.shutdown();
    }
}

/// Carry bytes both ways until either side ends, then close both.
pub fn start<P, R, W>(
    plain: P,
    mut reader: SecureReader<R>,
    mut writer: SecureWriter<W>,
) -> io::Result<horizon_os::thread::JoinHandle<()>>
where
    P: Plain,
    R: Secure,
    W: Secure,
{
    reader.get_ref().set_secure_timeout(None);
    writer.get_ref().set_secure_timeout(None);
    let mut plain_in = plain.try_clone_plain()?;
    let mut plain_out = plain;
    let outbound = horizon_os::thread::Builder::new()
        .name("horizon-nearby-out".into())
        .spawn(move || {
            let mut buf = vec![0u8; MAX_PAYLOAD];
            loop {
                match plain_in.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if writer.write_data(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            writer.get_ref().shutdown_secure();
            plain_in.shutdown_plain();
        })?;
    horizon_os::thread::Builder::new()
        .name("horizon-nearby-in".into())
        .spawn(move || {
            while let Ok(Some(frame)) = reader.read_frame() {
                if let Frame::Data(data) = frame
                    && plain_out.write_all(&data).is_err()
                {
                    break;
                }
            }
            plain_out.shutdown_plain();
            reader.get_ref().shutdown_secure();
            let _ = outbound.join();
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_friends::crypto::{self, Role};
    use horizon_protocol::connection::Pipe;
    use std::thread;

    #[test]
    fn the_secure_tunnel_carries_plain_bytes_both_ways() {
        let (game, pump_plain) = Pipe::pair();
        let (guest_net, host_net) = Pipe::pair();
        let secret = "0123456789abcdef";
        let session = 42;

        let host = thread::spawn(move || {
            let mut net = host_net;
            let key = crypto::exchange(&mut net, secret, "nearby", session).unwrap();
            let (reader, writer) = crypto::secure(net.try_clone(), net, &key, Role::Host).unwrap();
            start(pump_plain, reader, writer).unwrap()
        });
        let mut net = guest_net;
        let key = crypto::exchange(&mut net, secret, "nearby", session).unwrap();
        let (mut reader, mut writer) =
            crypto::secure(net.try_clone(), net, &key, Role::Guest).unwrap();

        let mut game = game;
        writer.write_data(b"from the tunnel").unwrap();
        let mut got = [0u8; 15];
        game.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"from the tunnel");

        game.write_all(b"from the game").unwrap();
        match reader.read_frame().unwrap().expect("a frame") {
            Frame::Data(data) => assert_eq!(data, b"from the game"),
            _ => panic!("not data"),
        }

        let pump = host.join().unwrap();
        drop(game);
        let _ = pump.join();
    }
}
