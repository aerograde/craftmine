//! One thread per public broker: connect, subscribe, publish presence and
//! signalling, keep the connection alive, reconnect with backoff.
use crate::mqtt::{self, Packet};
use horizon_os::time::{Duration, Instant};
use std::{
    io,
    sync::mpsc::{Receiver, Sender, TryRecvError},
};

/// What a link reports to the service.
pub enum Event {
    Connected(String),
    Disconnected(String, String),
    Publish { topic: String, payload: Vec<u8> },
}

/// What the service asks a link to publish.
pub enum Command {
    Publish {
        topic: String,
        payload: Vec<u8>,
        retain: bool,
    },
    Stop,
}

const KEEP_ALIVE: u16 = 30;
const PING_EVERY: Duration = Duration::from_secs(20);
const PING_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Start a link; it lives until `Stop`, the command sender drops, or the
/// service stops sending.
pub fn spawn(
    url: String,
    client_id: String,
    will_topic: String,
    subscriptions: Vec<String>,
    events: Sender<Event>,
    commands: Receiver<Command>,
) -> io::Result<horizon_os::thread::JoinHandle<()>> {
    horizon_os::thread::Builder::new()
        .name("horizon-nearby-broker".into())
        .spawn(move || run(url, client_id, will_topic, subscriptions, events, commands))
}

fn run(
    url: String,
    client_id: String,
    will_topic: String,
    subscriptions: Vec<String>,
    events: Sender<Event>,
    commands: Receiver<Command>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match serve(
            &url,
            &client_id,
            &will_topic,
            &subscriptions,
            &events,
            &commands,
        ) {
            Ok(()) => return,
            Err(e) => {
                if events
                    .send(Event::Disconnected(url.clone(), e.to_string()))
                    .is_err()
                {
                    return;
                }
            }
        }
        // Sleep in small pieces so a Stop is noticed promptly.
        let mut slept = Duration::ZERO;
        while slept < backoff {
            match commands.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Ok(Command::Publish { .. }) => {}
                Err(TryRecvError::Empty) => {}
            }
            horizon_os::thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

fn serve(
    url: &str,
    client_id: &str,
    will_topic: &str,
    subscriptions: &[String],
    events: &Sender<Event>,
    commands: &Receiver<Command>,
) -> io::Result<()> {
    let mut stream = mqtt::connect(url, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    Packet::Connect {
        client_id: client_id.to_owned(),
        keep_alive: KEEP_ALIVE,
        clean: true,
        will: Some((will_topic.to_owned(), Vec::new())),
        will_retain: true,
    }
    .write_to(&mut *stream)?;
    match Packet::read_from(&mut *stream)? {
        Packet::ConnAck { code: 0 } => {}
        Packet::ConnAck { code } => {
            return Err(io::Error::other(format!("the broker refused us: {code}")));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the broker did not say hello: {other:?}"),
            ));
        }
    }
    Packet::Subscribe {
        id: 1,
        filters: subscriptions.to_vec(),
    }
    .write_to(&mut *stream)?;
    if events.send(Event::Connected(url.to_owned())).is_err() {
        return Ok(());
    }

    let mut last_ping = Instant::now();
    let mut ping_sent: Option<Instant> = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Publish {
                    topic,
                    payload,
                    retain,
                }) => {
                    Packet::Publish {
                        topic,
                        payload,
                        retain,
                    }
                    .write_to(&mut *stream)?;
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => {
                    let _ = Packet::Disconnect.write_to(&mut *stream);
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if let Some(sent) = ping_sent
            && sent.elapsed() > PING_TIMEOUT
        {
            let _ = Packet::Disconnect.write_to(&mut *stream);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the broker stopped answering",
            ));
        }
        if last_ping.elapsed() >= PING_EVERY {
            Packet::PingReq.write_to(&mut *stream)?;
            last_ping = Instant::now();
            ping_sent = Some(Instant::now());
        }
        match Packet::read_from(&mut *stream) {
            Ok(Packet::Publish { topic, payload, .. }) => {
                if events.send(Event::Publish { topic, payload }).is_err() {
                    return Ok(());
                }
            }
            Ok(Packet::PingResp) => ping_sent = None,
            Ok(Packet::Other(_)) | Ok(Packet::SubAck { .. }) | Ok(Packet::PingReq) => {}
            Ok(Packet::Disconnect) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the broker closed the session",
                ));
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
}
