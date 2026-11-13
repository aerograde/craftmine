//! Two peers of one process, over 127.0.0.1, carrying a megabyte.
use horizon_os::time::Duration;
use horizon_webrtc::{Config, Event, Peer};
use std::{
    io::{Read, Write},
    thread,
    time::Instant,
};

fn config() -> Config {
    Config {
        stun: Vec::new(),
        gather: Duration::from_millis(200),
        connect: Duration::from_secs(10),
        loopback: true,
    }
}

/// The first event matching `want`, or a panic when the peer closes or the
/// deadline passes. `Peer::poll` hands each event out once.
fn wait_for(peer: &Peer, want: fn(&Event) -> bool, timeout: Duration) -> Event {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in peer.poll() {
            if want(&event) {
                return event;
            }
            if let Event::Closed(reason) = event {
                panic!("peer closed: {reason:?}");
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("event never arrived");
}

#[test]
fn the_channel_carries_bytes_both_ways() {
    let offerer = Peer::offer(config()).unwrap();
    let Event::LocalOffer(offer) = wait_for(
        &offerer,
        |e| matches!(e, Event::LocalOffer(_)),
        Duration::from_secs(5),
    ) else {
        unreachable!()
    };
    let answerer = Peer::answer(config(), offer).unwrap();
    let Event::LocalAnswer(answer) = wait_for(
        &answerer,
        |e| matches!(e, Event::LocalAnswer(_)),
        Duration::from_secs(5),
    ) else {
        unreachable!()
    };
    offerer.accept_answer(answer);
    wait_for(
        &offerer,
        |e| matches!(e, Event::Connected),
        Duration::from_secs(10),
    );
    wait_for(
        &answerer,
        |e| matches!(e, Event::Connected),
        Duration::from_secs(10),
    );

    let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect();
    let mut tx = offerer.channel();
    let mut rx = answerer.channel();
    let expected = payload.clone();
    let want = expected.len();
    let reader = thread::spawn(move || {
        let mut got = Vec::with_capacity(want);
        let mut buf = [0u8; 32768];
        while got.len() < want {
            match rx.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => panic!("read failed: {e}"),
            }
        }
        got
    });
    for chunk in payload.chunks(16 * 1024) {
        tx.write_all(chunk).unwrap();
    }
    let got = reader.join().unwrap();
    assert_eq!(got.len(), expected.len());
    assert_eq!(got, expected);
}
