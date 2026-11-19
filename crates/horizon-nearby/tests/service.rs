//! Two services, one broker: discovery, a join request and acceptance, a
//! WebRTC offer and answer over MQTT, the SPAKE2 tunnel, and the two memory
//! pipes the game would use. No game assets and no network.
use horizon_nearby::{Command, Config, Event, Policy, Service, mqtt::Packet};
use horizon_protocol::connection::Pipe;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct FakeBroker {
    port: u16,
    stop: Arc<AtomicBool>,
}

struct Client {
    write: TcpStream,
    filters: Arc<Mutex<Vec<String>>>,
}

struct State {
    retained: HashMap<String, Vec<u8>>,
    clients: Vec<Client>,
}

impl FakeBroker {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a broker port");
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::spawn(move || {
            let state = Arc::new(Mutex::new(State {
                retained: HashMap::new(),
                clients: Vec::new(),
            }));
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let state = state.clone();
                thread::spawn(move || client(stream, state));
            }
        });
        Self { port, stop }
    }
}

impl Drop for FakeBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn send(packet: &Packet, stream: &TcpStream) {
    let mut writer = stream;
    let _ = packet.write_to(&mut writer);
}

fn client(stream: TcpStream, state: Arc<Mutex<State>>) {
    let Ok(mut read) = stream.try_clone() else {
        return;
    };
    let filters = Arc::new(Mutex::new(Vec::new()));
    let mut will: Option<(String, Vec<u8>, bool)> = None;
    let mut registered = false;
    while let Ok(packet) = Packet::read_from(&mut read) {
        match packet {
            Packet::Connect {
                will: connect_will,
                will_retain,
                ..
            } => {
                will = connect_will.map(|(topic, payload)| (topic, payload, will_retain));
                send(&Packet::ConnAck { code: 0 }, &stream);
                state.lock().unwrap().clients.push(Client {
                    write: stream.try_clone().unwrap(),
                    filters: filters.clone(),
                });
                registered = true;
            }
            Packet::Subscribe {
                id,
                filters: wanted,
            } => {
                {
                    let mut own = filters.lock().unwrap();
                    own.extend(wanted.clone());
                }
                send(
                    &Packet::SubAck {
                        id,
                        codes: vec![0; wanted.len()],
                    },
                    &stream,
                );
                let retained: Vec<(String, Vec<u8>)> = {
                    let state = state.lock().unwrap();
                    let mut found: Vec<(String, Vec<u8>)> = state
                        .retained
                        .iter()
                        .filter(|(topic, _)| wanted.iter().any(|f| matches(f, topic)))
                        .map(|(topic, payload)| (topic.clone(), payload.clone()))
                        .collect();
                    found.sort();
                    found
                };
                for (topic, payload) in retained {
                    send(
                        &Packet::Publish {
                            topic,
                            payload,
                            retain: true,
                        },
                        &stream,
                    );
                }
            }
            Packet::Publish {
                topic,
                payload,
                retain,
            } => publish(&state, &topic, &payload, retain),
            Packet::PingReq => send(&Packet::PingResp, &stream),
            Packet::Disconnect => break,
            _ => {}
        }
    }
    {
        let mut state = state.lock().unwrap();
        state
            .clients
            .retain(|client| !Arc::ptr_eq(&client.filters, &filters));
    }
    if registered && let Some((topic, payload, retain)) = will {
        publish(&state, &topic, &payload, retain);
    }
}

fn publish(state: &Arc<Mutex<State>>, topic: &str, payload: &[u8], retain: bool) {
    let mut state = state.lock().unwrap();
    if retain {
        state.retained.insert(topic.to_owned(), payload.to_vec());
    }
    let packet = Packet::Publish {
        topic: topic.to_owned(),
        payload: payload.to_vec(),
        retain,
    };
    for client in &mut state.clients {
        if client
            .filters
            .lock()
            .unwrap()
            .iter()
            .any(|filter| matches(filter, topic))
        {
            send(&packet, &client.write);
        }
    }
}

fn matches(filter: &str, topic: &str) -> bool {
    let filter: Vec<&str> = filter.split('/').collect();
    let topic: Vec<&str> = topic.split('/').collect();
    let mut at = 0;
    for (index, part) in filter.iter().enumerate() {
        match *part {
            "#" => return true,
            "+" => at += 1,
            other => {
                if topic.get(at) != Some(&other) {
                    return false;
                }
                at += 1;
            }
        }
        if at > topic.len() && index + 1 < filter.len() {
            return false;
        }
    }
    at == topic.len()
}

fn config(name: &str, port: u16) -> Config {
    Config {
        enabled: true,
        player_name: name.into(),
        proto: horizon_protocol::PROTOCOL_VERSION,
        game: horizon_protocol::GAME_VERSION.into(),
        policy: Policy::Ask,
        brokers: vec![format!("mqtt://127.0.0.1:{port}")],
        room: Some("testroom".into()),
        stun: Vec::new(),
        lan: false,
        ice_timeout: Duration::from_secs(3),
        connect_timeout: Duration::from_secs(30),
        loopback: true,
    }
}

fn wait_until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(value) = check() {
            return value;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn wait_event(service: &Service, what: &str, mut check: impl FnMut(&Event) -> bool) -> Event {
    wait_until(what, || {
        service.poll().into_iter().find(|event| check(event))
    })
}

#[test]
fn two_services_discover_request_accept_and_carry_pipes() {
    let broker = FakeBroker::start();
    let host = Service::start(config("Host", broker.port));
    let guest = Service::start(config("Guest", broker.port));

    host.command(Command::ShareWorld {
        name: "Test world".into(),
        players: 1,
        max: 8,
    });
    let world = wait_until("the guest to see the world", || {
        guest
            .snapshot()
            .worlds
            .into_iter()
            .find(|world| world.name == "Test world")
    });
    assert_eq!(world.host_name, "Host");

    guest.command(Command::RequestJoin {
        world_id: world.world_id.clone(),
    });
    let request = wait_event(&host, "the join request", |event| {
        matches!(event, Event::Request(_))
    });
    let Event::Request(request) = request else {
        unreachable!()
    };
    assert_eq!(request.guest_name, "Guest");

    host.command(Command::AcceptRequest {
        request_id: request.msg_id.clone(),
    });
    let needed = wait_event(&host, "the server pipe request", |event| {
        matches!(event, Event::ServerPipeNeeded { .. })
    });
    let Event::ServerPipeNeeded { session, .. } = needed else {
        unreachable!()
    };
    let (mut host_game, server_pipe) = Pipe::pair();
    host.command(Command::ProvideServerPipe {
        session,
        pipe: server_pipe,
    });

    let ready = wait_event(&guest, "the guest's secure pipe", |event| {
        matches!(event, Event::GuestReady { .. })
    });
    let Event::GuestReady { pipe, .. } = ready else {
        unreachable!()
    };
    let mut guest_game = pipe;

    let from_host = b"hello from the host";
    host_game.write_all(from_host).unwrap();
    let mut got = vec![0u8; from_host.len()];
    guest_game.read_exact(&mut got).unwrap();
    assert_eq!(got.as_slice(), from_host);

    let from_guest = b"hello from the guest";
    guest_game.write_all(from_guest).unwrap();
    let mut got = vec![0u8; from_guest.len()];
    host_game.read_exact(&mut got).unwrap();
    assert_eq!(got.as_slice(), from_guest);
}

#[test]
fn a_host_invites_a_nearby_player_and_the_player_joins() {
    let broker = FakeBroker::start();
    let host = Service::start(config("Host", broker.port));
    let guest = Service::start(config("Guest", broker.port));
    host.command(Command::ShareWorld {
        name: "Invite world".into(),
        players: 1,
        max: 8,
    });
    let person = wait_until("the host to see the guest", || {
        host.snapshot()
            .people
            .into_iter()
            .find(|person| person.name == "Guest")
    });
    host.command(Command::Invite {
        player_id: person.id.clone(),
    });
    let invite = wait_event(&guest, "the invite", |event| {
        matches!(event, Event::Invite(_))
    });
    let Event::Invite(invite) = invite else {
        unreachable!()
    };
    guest.command(Command::AcceptInvite {
        invite_id: invite.msg_id.clone(),
    });

    let needed = wait_event(&host, "the server pipe request", |event| {
        matches!(event, Event::ServerPipeNeeded { .. })
    });
    let Event::ServerPipeNeeded { session, .. } = needed else {
        unreachable!()
    };
    let (mut host_game, server_pipe) = Pipe::pair();
    host.command(Command::ProvideServerPipe {
        session,
        pipe: server_pipe,
    });
    let ready = wait_event(&guest, "the guest's secure pipe", |event| {
        matches!(event, Event::GuestReady { .. })
    });
    let Event::GuestReady { pipe, .. } = ready else {
        unreachable!()
    };
    let mut guest_game = pipe;
    host_game.write_all(b"invite").unwrap();
    let mut got = [0u8; 6];
    guest_game.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"invite");
}

#[test]
fn two_guests_get_independent_links_to_the_host() {
    let broker = FakeBroker::start();
    let host = Service::start(config("Host", broker.port));
    let a = Service::start(config("Guest A", broker.port));
    let b = Service::start(config("Guest B", broker.port));
    host.command(Command::ShareWorld {
        name: "Mesh world".into(),
        players: 1,
        max: 8,
    });
    let world = wait_until("the world", || {
        a.snapshot()
            .worlds
            .into_iter()
            .find(|world| world.name == "Mesh world")
    });
    a.command(Command::RequestJoin {
        world_id: world.world_id.clone(),
    });
    b.command(Command::RequestJoin {
        world_id: world.world_id.clone(),
    });
    let mut requests = Vec::new();
    wait_until("both requests", || {
        for event in host.poll() {
            if let Event::Request(request) = event {
                requests.push(request);
            }
        }
        (requests.len() == 2).then_some(())
    });
    for request in &requests {
        host.command(Command::AcceptRequest {
            request_id: request.msg_id.clone(),
        });
    }
    let mut pipes: Vec<(String, Pipe)> = Vec::new();
    wait_until("both server pipe requests", || {
        for event in host.poll() {
            if let Event::ServerPipeNeeded { session, guest } = event {
                let (game, server) = Pipe::pair();
                host.command(Command::ProvideServerPipe {
                    session,
                    pipe: server,
                });
                pipes.push((guest, game));
            }
        }
        (pipes.len() == 2).then_some(())
    });
    let ready_a = wait_event(&a, "guest A's pipe", |event| {
        matches!(event, Event::GuestReady { .. })
    });
    let ready_b = wait_event(&b, "guest B's pipe", |event| {
        matches!(event, Event::GuestReady { .. })
    });
    let Event::GuestReady { pipe: pipe_a, .. } = ready_a else {
        unreachable!()
    };
    let Event::GuestReady { pipe: pipe_b, .. } = ready_b else {
        unreachable!()
    };
    let mut guest_a = pipe_a;
    let mut guest_b = pipe_b;
    let mut to_a = pipes
        .iter()
        .position(|(guest, _)| guest == "Guest A")
        .map(|index| pipes.remove(index).1)
        .unwrap();
    let mut to_b = pipes
        .iter()
        .position(|(guest, _)| guest == "Guest B")
        .map(|index| pipes.remove(index).1)
        .unwrap();

    to_a.write_all(b"for A").unwrap();
    to_b.write_all(b"for B").unwrap();
    let mut got = [0u8; 5];
    guest_a.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"for A");
    guest_b.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"for B");

    let sessions = host.snapshot().sessions;
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .all(|session| session.hosting && session.connected)
    );
}

/// The feature the browser build needed: with no click, a client joins the
/// open world in its room, and is handed the same secure pipe a manual join
/// produces. `AutoJoin` is armed before the world is announced, so the arrival
/// has to be what triggers it.
#[test]
fn auto_join_carries_a_guest_into_an_open_world() {
    let broker = FakeBroker::start();
    let mut host_config = config("Host", broker.port);
    host_config.policy = Policy::Open;
    let host = Service::start(host_config);
    let guest = Service::start(config("Guest", broker.port));

    guest.command(Command::AutoJoin);
    // The world is shared only now: the join must follow discovery, not the
    // command, or the test would only prove the empty case.
    host.command(Command::ShareWorld {
        name: "Auto world".into(),
        players: 1,
        max: 8,
    });

    let needed = wait_event(&host, "the server pipe request", |event| {
        matches!(event, Event::ServerPipeNeeded { .. })
    });
    let Event::ServerPipeNeeded { session, .. } = needed else {
        unreachable!()
    };
    let (mut host_game, server_pipe) = Pipe::pair();
    host.command(Command::ProvideServerPipe {
        session,
        pipe: server_pipe,
    });

    let ready = wait_event(&guest, "the guest's secure pipe", |event| {
        matches!(event, Event::GuestReady { .. })
    });
    let Event::GuestReady { mut pipe, host, .. } = ready else {
        unreachable!()
    };
    assert_eq!(host, "Host", "the pipe names the host it joined, not the world");
    host_game.write_all(b"joined").unwrap();
    let mut got = [0u8; 6];
    pipe.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"joined");
}

/// An `Ask` world needs its host to accept, which nobody is there to do, so an
/// automatic join must not sit on it — it waits for an open one instead.
#[test]
fn auto_join_waits_for_an_open_world_rather_than_an_ask_one() {
    let broker = FakeBroker::start();
    let host = Service::start(config("Host", broker.port));
    let guest = Service::start(config("Guest", broker.port));

    guest.command(Command::AutoJoin);
    host.command(Command::ShareWorld {
        name: "Ask world".into(),
        players: 1,
        max: 8,
    });
    let world = wait_until("the guest to see the world", || {
        guest
            .snapshot()
            .worlds
            .into_iter()
            .find(|world| world.name == "Ask world")
    });
    assert!(world.policy == Policy::Ask);

    // No request is made, so the host never sees one.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert!(
            !host.poll().iter().any(|e| matches!(e, Event::Request(_))),
            "auto-join asked a host who is not at the screen"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
