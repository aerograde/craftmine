//! The nearby state machine: what the game sees, and what happens between
//! presence, join requests, invites and the WebRTC session threads.
use crate::{
    broker::{self, Command as BrokerCommand, Event as BrokerEvent},
    lan::{Incoming as LanIncoming, Lan},
    proto::{
        self, Envelope, Invite, JoinAccepted, JoinDenied, JoinRequest, PlayerStatus, Policy,
        Presence, PresenceKind, SessionKey, Signal, StaticKey, VERSION,
    },
    room,
    session::{self, Command as SessionCommand, Event as SessionEvent},
    transport::Peer,
};
use horizon_os::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
};

const PRESENCE_EVERY: Duration = Duration::from_secs(10);
const PRESENCE_TTL: Duration = Duration::from_secs(35);
const REQUEST_TTL: Duration = Duration::from_secs(60);
const INVITE_TTL: Duration = Duration::from_secs(120);
const SESSION_TTL: Duration = Duration::from_secs(90);
const EXPIRE_EVERY: Duration = Duration::from_secs(1);
const SEEN_TTL: Duration = Duration::from_secs(120);
const MAX_REQUESTS_PER_MINUTE: usize = 5;

/// What the client starts the service with.
#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool,
    pub player_name: String,
    pub proto: i32,
    pub game: String,
    pub policy: Policy,
    pub brokers: Vec<String>,
    /// A custom room code; `None` uses the public-address room.
    pub room: Option<String>,
    pub stun: Vec<String>,
    pub lan: bool,
    pub ice_timeout: Duration,
    pub connect_timeout: Duration,
    /// Advertise 127.0.0.1 candidates (tests on one machine).
    pub loopback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            player_name: "Player".into(),
            proto: 777,
            game: horizon_protocol::GAME_VERSION.into(),
            policy: Policy::Ask,
            brokers: default_brokers(),
            room: None,
            stun: default_stun(),
            lan: true,
            ice_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(30),
            loopback: false,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn default_brokers() -> Vec<String> {
    vec![
        "mqtts://broker.emqx.io:8883".into(),
        "mqtts://test.mosquitto.org:8883".into(),
        "mqtts://mqtt.eclipseprojects.io:8883".into(),
    ]
}
#[cfg(target_arch = "wasm32")]
pub fn default_brokers() -> Vec<String> {
    vec![
        "wss://broker.emqx.io:8084/mqtt".into(),
        "wss://test.mosquitto.org:8081".into(),
        "wss://mqtt.eclipseprojects.io:443/mqtt".into(),
    ]
}

pub fn default_stun() -> Vec<String> {
    vec![
        "stun.l.google.com:19302".into(),
        "stun1.l.google.com:19302".into(),
        "stun2.l.google.com:19302".into(),
        "stun.cloudflare.com:3478".into(),
    ]
}

/// The client's commands, applied on the service thread.
pub enum Command {
    ShareWorld {
        name: String,
        players: usize,
        max: usize,
    },
    StopShare,
    SetPolicy(Policy),
    SetPlayerStatus(PlayerStatus),
    RequestJoin {
        world_id: String,
    },
    AcceptRequest {
        request_id: String,
    },
    DenyRequest {
        request_id: String,
    },
    Invite {
        player_id: String,
    },
    AcceptInvite {
        invite_id: String,
    },
    ProvideServerPipe {
        session: u64,
        pipe: horizon_protocol::connection::Pipe,
    },
    SetRoom(Option<String>),
    Shutdown,
}

/// What the client needs to see immediately.
pub enum Event {
    SnapshotChanged,
    Request(JoinRequest),
    Invite(Invite),
    ServerPipeNeeded {
        session: u64,
        guest: String,
    },
    GuestReady {
        session: u64,
        pipe: horizon_protocol::connection::Pipe,
        host: String,
    },
    SessionClosed {
        session: u64,
        reason: Option<String>,
    },
    Notice(String),
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub enabled: bool,
    pub room: Option<String>,
    pub policy: Policy,
    pub brokers: Vec<BrokerStatus>,
    pub sharing: Option<SharedWorld>,
    pub worlds: Vec<WorldRow>,
    pub people: Vec<PersonRow>,
    pub requests: Vec<RequestRow>,
    pub invites: Vec<InviteRow>,
    pub sessions: Vec<SessionRow>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BrokerStatus {
    pub url: String,
    pub state: String,
}
#[derive(Clone, Debug)]
pub struct SharedWorld {
    pub world_id: String,
    pub name: String,
    pub players: u8,
    pub max: u8,
}
#[derive(Clone, Debug)]
pub struct WorldRow {
    pub world_id: String,
    pub name: String,
    pub host_id: String,
    pub host_name: String,
    pub players: u8,
    pub max: u8,
    pub policy: Policy,
    pub compatible: bool,
    pub direct: bool,
}
#[derive(Clone, Debug)]
pub struct PersonRow {
    pub id: String,
    pub name: String,
    pub status: PlayerStatus,
    pub compatible: bool,
    pub direct: bool,
}
#[derive(Clone, Debug)]
pub struct RequestRow {
    pub id: String,
    pub guest_name: String,
    pub world_name: String,
}
#[derive(Clone, Debug)]
pub struct InviteRow {
    pub id: String,
    pub host_name: String,
    pub world_name: String,
}
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub session: u64,
    pub peer: String,
    pub hosting: bool,
    pub connected: bool,
}

/// The handle the game keeps.
pub struct Service {
    commands: Sender<Command>,
    events: Receiver<Event>,
    snapshot: Arc<Mutex<Snapshot>>,
    _thread: Option<horizon_os::thread::JoinHandle<()>>,
}

impl Service {
    /// Start the service, or a disabled one when the player turned it off
    /// or the build has no networking.
    pub fn start(config: Config) -> Self {
        if !config.enabled || !networking() {
            return Self::disabled();
        }
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(Snapshot {
            enabled: true,
            policy: config.policy,
            room: config.room.clone(),
            notice: Some("Looking for nearby players…".into()),
            ..Snapshot::default()
        }));
        let write = snapshot.clone();
        let thread = horizon_os::thread::Builder::new()
            .name("horizon-nearby".into())
            .spawn(move || run(config, command_rx, event_tx, write))
            .ok();
        if thread.is_none()
            && let Ok(mut snap) = horizon_os::sync::lock(&snapshot)
        {
            snap.enabled = false;
            snap.notice = Some("The nearby service could not start".into());
        }
        Self {
            commands,
            events,
            snapshot,
            _thread: thread,
        }
    }

    pub fn disabled() -> Self {
        let (commands, _rx) = mpsc::channel();
        let (_tx, events) = mpsc::channel();
        Self {
            commands,
            events,
            snapshot: Arc::new(Mutex::new(Snapshot::default())),
            _thread: None,
        }
    }

    pub fn available(&self) -> bool {
        self.snapshot().enabled
    }

    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn snapshot(&self) -> Snapshot {
        horizon_os::sync::lock(&self.snapshot)
            .map(|snap| snap.clone())
            .unwrap_or_default()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

fn networking() -> bool {
    cfg!(not(target_arch = "wasm32")) || cfg!(target_feature = "atomics")
}

fn run(
    config: Config,
    commands: Receiver<Command>,
    events: Sender<Event>,
    snapshot: Arc<Mutex<Snapshot>>,
) {
    let mut inner = match Inner::new(config, events, snapshot) {
        Ok(inner) => inner,
        Err(e) => {
            log::warn!("nearby: {e}");
            return;
        }
    };
    inner.start();
    loop {
        inner.drain_commands(&commands);
        if inner.stop {
            break;
        }
        inner.drain_lan();
        inner.drain_brokers();
        inner.drain_sessions();
        if inner.last_presence.elapsed() >= PRESENCE_EVERY {
            inner.publish_presence();
        }
        if inner.last_expire.elapsed() >= EXPIRE_EVERY {
            inner.expire();
            inner.last_expire = Instant::now();
        }
        if inner.dirty {
            inner.write_snapshot();
        }
        horizon_os::thread::sleep(Duration::from_millis(50));
    }
    inner.shutdown();
}

#[derive(Clone)]
struct Seen<T> {
    value: T,
    lan: Option<SocketAddr>,
    seen: Instant,
}

struct Sharing {
    presence: Presence,
    players: u8,
    max: u8,
}

struct RequestPending {
    request: JoinRequest,
    world_name: String,
    seen: Instant,
}

struct InviteSeen {
    invite: Invite,
    seen: Instant,
}

struct GuestPending {
    host_id: String,
    host_name: String,
    world_id: String,
    eph: StaticKey,
    seen: Instant,
}

struct HostPending {
    guest_id: String,
    key: SessionKey,
    token: String,
    pipe: Option<horizon_protocol::connection::Pipe>,
    seen: Instant,
}

struct Live {
    peer_name: String,
    hosting: bool,
    connected: bool,
    commands: Option<Sender<SessionCommand>>,
    seen: Instant,
}

struct LinkHandle {
    url: String,
    commands: Sender<BrokerCommand>,
    state: String,
}

#[derive(Default)]
struct SeenIds {
    set: HashSet<String>,
    order: VecDeque<(String, Instant)>,
}
impl SeenIds {
    fn insert(&mut self, id: &str) -> bool {
        if id.is_empty() || !self.set.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back((id.to_owned(), Instant::now()));
        if self.order.len() > 512
            && let Some((old, _)) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
    fn expire(&mut self) {
        while let Some((_, at)) = self.order.front() {
            if at.elapsed() < SEEN_TTL {
                break;
            }
            let (old, _) = self.order.pop_front().expect("front");
            self.set.remove(&old);
        }
    }
}

struct Inner {
    config: Config,
    id: String,
    key: StaticKey,
    public_key: String,
    room: Option<String>,
    lan: Option<Lan>,
    lan_rx: Option<Receiver<LanIncoming>>,
    links: Vec<LinkHandle>,
    broker_rx: Receiver<BrokerEvent>,
    broker_tx: Sender<BrokerEvent>,
    session_rx: Receiver<SessionEvent>,
    session_tx: Sender<SessionEvent>,
    events: Sender<Event>,
    snapshot: Arc<Mutex<Snapshot>>,
    worlds: HashMap<String, Seen<Presence>>,
    people: HashMap<String, Seen<Presence>>,
    requests: Vec<RequestPending>,
    invites: Vec<InviteSeen>,
    issued: HashMap<String, (String, Instant)>,
    request_times: HashMap<String, VecDeque<Instant>>,
    guest: Option<GuestPending>,
    guest_keys: HashMap<u64, SessionKey>,
    host_pending: HashMap<u64, HostPending>,
    live: HashMap<u64, Live>,
    sharing: Option<Sharing>,
    status: PlayerStatus,
    seen: SeenIds,
    next_session: u64,
    last_presence: Instant,
    last_expire: Instant,
    dirty: bool,
    stop: bool,
    notice: Option<String>,
}

impl Inner {
    fn new(
        config: Config,
        events: Sender<Event>,
        snapshot: Arc<Mutex<Snapshot>>,
    ) -> io::Result<Self> {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (session_tx, session_rx) = mpsc::channel();
        let key = StaticKey::generate()?;
        let public_key = key.public();
        let id = proto::random_id()?;
        let mut seed = [0u8; 8];
        proto::random_fill(&mut seed)?;
        Ok(Self {
            config,
            id,
            key,
            public_key,
            room: None,
            lan: None,
            lan_rx: None,
            links: Vec::new(),
            broker_rx,
            broker_tx,
            session_rx,
            session_tx,
            events,
            snapshot,
            worlds: HashMap::new(),
            people: HashMap::new(),
            requests: Vec::new(),
            invites: Vec::new(),
            issued: HashMap::new(),
            request_times: HashMap::new(),
            guest: None,
            guest_keys: HashMap::new(),
            host_pending: HashMap::new(),
            live: HashMap::new(),
            sharing: None,
            status: PlayerStatus::Menu,
            seen: SeenIds::default(),
            next_session: u64::from_be_bytes(seed).max(1),
            last_presence: Instant::now(),
            last_expire: Instant::now(),
            dirty: true,
            stop: false,
            notice: None,
        })
    }

    fn start(&mut self) {
        // LAN first: it needs no room, so a network with no internet still
        // sees its worlds while STUN is timing out.
        if self.config.lan {
            let (tx, rx) = mpsc::channel();
            match Lan::start(tx) {
                Ok(lan) => {
                    self.lan = Some(lan);
                    self.lan_rx = Some(rx);
                }
                Err(e) => log::debug!("nearby: no local-network discovery: {e}"),
            }
        }
        let public = probe_public_ip(&self.config.stun);
        self.room = room::resolve(self.config.room.as_deref(), public);
        if self.room.is_none() {
            self.notice = Some("No nearby room — local network only".into());
        }
        self.start_brokers();
        self.publish_presence();
        self.write_snapshot();
    }

    fn start_brokers(&mut self) {
        let Some(room) = self.room.clone() else {
            return;
        };
        let subscriptions = vec![
            format!("horizon/nearby/v1/{room}/presence/+"),
            format!("horizon/nearby/v1/{room}/inbox/{}", self.id),
        ];
        let will = format!("horizon/nearby/v1/{room}/presence/{}", self.id);
        for url in self.config.brokers.clone() {
            let (tx, rx) = mpsc::channel();
            let spawned = broker::spawn(
                url.clone(),
                format!("hzn{}", self.id),
                will.clone(),
                subscriptions.clone(),
                self.broker_tx.clone(),
                rx,
            );
            self.links.push(LinkHandle {
                url,
                commands: tx,
                state: if spawned.is_ok() {
                    "connecting".into()
                } else {
                    "unavailable".into()
                },
            });
        }
    }

    fn stop_brokers(&mut self) {
        for link in self.links.drain(..) {
            let _ = link.commands.send(BrokerCommand::Stop);
        }
    }

    fn share_world(&mut self, name: String, players: usize, max: usize) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let players = players.clamp(1, 255) as u8;
        let max = max.clamp(1, 255) as u8;
        if let Some(sharing) = &mut self.sharing
            && sharing.presence.name == name
        {
            sharing.players = players;
            sharing.max = max;
            sharing.presence.players = Some(players);
            sharing.presence.max = Some(max);
            sharing.presence.policy = Some(self.config.policy);
            sharing.presence.ts = proto::now_ms();
            self.publish_presence();
            self.dirty = true;
            return;
        }
        self.stop_share();
        let Ok(world_id) = proto::random_id() else {
            return;
        };
        let presence = Presence {
            v: VERSION,
            kind: PresenceKind::World,
            id: world_id.clone(),
            name: name.clone(),
            host_id: Some(self.id.clone()),
            world_id: Some(world_id.clone()),
            players: Some(players),
            max: Some(max),
            policy: Some(self.config.policy),
            status: PlayerStatus::Hosting,
            key: self.public_key.clone(),
            proto: self.config.proto,
            game: self.config.game.clone(),
            room: self.room.clone().unwrap_or_default(),
            ts: proto::now_ms(),
        };
        self.sharing = Some(Sharing {
            presence,
            players,
            max,
        });
        self.publish_presence();
        self.dirty = true;
    }

    fn stop_share(&mut self) {
        let Some(sharing) = self.sharing.take() else {
            return;
        };
        if let Some(room) = &self.room {
            let topic = format!("horizon/nearby/v1/{room}/presence/{}", sharing.presence.id);
            for link in &self.links {
                let _ = link.commands.send(BrokerCommand::Publish {
                    topic: topic.clone(),
                    payload: Vec::new(),
                    retain: true,
                });
            }
        }
        self.dirty = true;
    }

    fn presence(&self, status: PlayerStatus) -> Presence {
        Presence {
            v: VERSION,
            kind: PresenceKind::Player,
            id: self.id.clone(),
            name: self.config.player_name.clone(),
            host_id: None,
            world_id: None,
            players: None,
            max: None,
            policy: None,
            status,
            key: self.public_key.clone(),
            proto: self.config.proto,
            game: self.config.game.clone(),
            room: self.room.clone().unwrap_or_default(),
            ts: proto::now_ms(),
        }
    }

    fn publish_presence(&mut self) {
        let status = if self.sharing.is_some() {
            PlayerStatus::Hosting
        } else {
            self.status
        };
        let player = self.presence(status);
        self.announce(&player, true);
        if let Some(sharing) = &self.sharing {
            let mut world = sharing.presence.clone();
            world.players = Some(sharing.players);
            world.max = Some(sharing.max);
            world.policy = Some(self.config.policy);
            world.room = self.room.clone().unwrap_or_default();
            world.ts = proto::now_ms();
            self.announce(&world, true);
        }
        self.last_presence = Instant::now();
    }

    fn announce(&self, presence: &Presence, retain: bool) {
        if let Some(lan) = &self.lan {
            lan.publish(presence);
        }
        let Some(room) = &self.room else {
            return;
        };
        let Ok(payload) = serde_json::to_vec(presence) else {
            return;
        };
        let topic = format!("horizon/nearby/v1/{room}/presence/{}", presence.id);
        for link in &self.links {
            let _ = link.commands.send(BrokerCommand::Publish {
                topic: topic.clone(),
                payload: payload.clone(),
                retain,
            });
        }
    }

    fn send_envelope(&self, to: &str, envelope: &Envelope, lan: Option<SocketAddr>) {
        if let Some(lan) = lan
            && let Some(socket) = &self.lan
        {
            socket.signal(lan, envelope);
        }
        let Some(room) = &self.room else {
            return;
        };
        let Ok(payload) = serde_json::to_vec(envelope) else {
            return;
        };
        let topic = format!("horizon/nearby/v1/{room}/inbox/{to}");
        for link in &self.links {
            let _ = link.commands.send(BrokerCommand::Publish {
                topic: topic.clone(),
                payload: payload.clone(),
                retain: false,
            });
        }
    }

    fn drain_commands(&mut self, commands: &Receiver<Command>) {
        loop {
            match commands.try_recv() {
                Ok(Command::ShareWorld { name, players, max }) => {
                    self.share_world(name, players, max)
                }
                Ok(Command::StopShare) => self.stop_share(),
                Ok(Command::SetPolicy(policy)) => {
                    self.config.policy = policy;
                    if let Some(sharing) = &mut self.sharing {
                        sharing.presence.policy = Some(policy);
                    }
                    self.publish_presence();
                    self.dirty = true;
                }
                Ok(Command::SetPlayerStatus(status)) => {
                    self.status = status;
                    self.publish_presence();
                    self.dirty = true;
                }
                Ok(Command::RequestJoin { world_id }) => self.request_join(world_id),
                Ok(Command::AcceptRequest { request_id }) => self.accept_request(&request_id),
                Ok(Command::DenyRequest { request_id }) => self.deny_request(&request_id),
                Ok(Command::Invite { player_id }) => self.invite_player(&player_id),
                Ok(Command::AcceptInvite { invite_id }) => self.accept_invite(&invite_id),
                Ok(Command::ProvideServerPipe { session, pipe }) => {
                    self.provide_pipe(session, pipe)
                }
                Ok(Command::SetRoom(code)) => self.set_room(code),
                Ok(Command::Shutdown) => {
                    self.stop = true;
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.stop = true;
                    return;
                }
            }
        }
    }

    fn request_join(&mut self, world_id: String) {
        let Some(world) = self.worlds.get(&world_id).cloned() else {
            self.notify("That world is no longer nearby");
            return;
        };
        if !compatible(self, &world.value) {
            self.notify("That world is from another Horizon version");
            return;
        }
        let Some(host_id) = world.value.host_id.clone() else {
            self.notify("That world did not say who hosts it");
            return;
        };
        let Ok(eph) = StaticKey::generate() else {
            return;
        };
        let request = JoinRequest {
            v: VERSION,
            msg_id: proto::random_id().unwrap_or_default(),
            ts: proto::now_ms(),
            world_id: world_id.clone(),
            guest_id: self.id.clone(),
            guest_name: self.config.player_name.clone(),
            eph: eph.public(),
            token: None,
        };
        let Ok(envelope) = self.key.seal(
            &self.id,
            &host_id,
            &world.value.key,
            &Signal::JoinRequest(request),
        ) else {
            return;
        };
        self.send_envelope(&host_id, &envelope, world.lan);
        self.guest = Some(GuestPending {
            host_id,
            host_name: world.value.name.clone(),
            world_id,
            eph,
            seen: Instant::now(),
        });
        self.notice = Some("Join request sent".into());
        self.dirty = true;
    }

    fn accept_invite(&mut self, invite_id: &str) {
        let Some(index) = self
            .invites
            .iter()
            .position(|i| i.invite.msg_id == invite_id)
        else {
            return;
        };
        let invite = self.invites.remove(index).invite;
        let Ok(eph) = StaticKey::generate() else {
            return;
        };
        let request = JoinRequest {
            v: VERSION,
            msg_id: proto::random_id().unwrap_or_default(),
            ts: proto::now_ms(),
            world_id: invite.world_id.clone(),
            guest_id: self.id.clone(),
            guest_name: self.config.player_name.clone(),
            eph: eph.public(),
            token: Some(invite.token.clone()),
        };
        let Ok(envelope) = self.key.seal(
            &self.id,
            &invite.host_id,
            &invite.host_key,
            &Signal::JoinRequest(request),
        ) else {
            return;
        };
        let lan = self.people.get(&invite.host_id).and_then(|p| p.lan);
        self.send_envelope(&invite.host_id, &envelope, lan);
        self.guest = Some(GuestPending {
            host_id: invite.host_id,
            host_name: invite.host_name,
            world_id: invite.world_id,
            eph,
            seen: Instant::now(),
        });
        self.notice = Some("Joining the invite…".into());
        self.dirty = true;
    }

    fn invite_player(&mut self, player_id: &str) {
        let Some((world_id, world_name)) = self
            .sharing
            .as_ref()
            .map(|sharing| (sharing.presence.id.clone(), sharing.presence.name.clone()))
        else {
            self.notify("Share your world first");
            return;
        };
        let Some(person) = self.people.get(player_id).cloned() else {
            self.notify("That player is no longer nearby");
            return;
        };
        let Ok(token) = proto::random_token() else {
            return;
        };
        self.issued
            .insert(token.clone(), (world_id.clone(), Instant::now()));
        let invite = Invite {
            v: VERSION,
            msg_id: proto::random_id().unwrap_or_default(),
            ts: proto::now_ms(),
            host_id: self.id.clone(),
            host_name: self.config.player_name.clone(),
            world_id,
            world_name,
            token,
            host_key: self.public_key.clone(),
            expires: proto::now_ms() + 120_000,
        };
        let Ok(envelope) = self.key.seal(
            &self.id,
            player_id,
            &person.value.key,
            &Signal::Invite(invite),
        ) else {
            return;
        };
        self.send_envelope(player_id, &envelope, person.lan);
        self.notify(&format!("Invited {}", person.value.name));
    }

    fn accept_request(&mut self, request_id: &str) {
        let Some(index) = self
            .requests
            .iter()
            .position(|r| r.request.msg_id == request_id)
        else {
            return;
        };
        let pending = self.requests.remove(index);
        self.accept_request_data(pending.request, pending.world_name);
    }

    fn deny_request(&mut self, request_id: &str) {
        let Some(index) = self
            .requests
            .iter()
            .position(|r| r.request.msg_id == request_id)
        else {
            return;
        };
        let pending = self.requests.remove(index);
        self.deny(
            &pending.request.guest_id,
            &pending.request.world_id,
            proto::DenyReason::Denied,
            None,
        );
        self.dirty = true;
    }

    fn accept_request_data(&mut self, request: JoinRequest, _world_name: String) {
        let Some(sharing) = &self.sharing else {
            return;
        };
        let world_id = sharing.presence.id.clone();
        let guest = self.people.get(&request.guest_id).cloned();
        let Some(guest) = guest else {
            self.deny(
                &request.guest_id,
                &world_id,
                proto::DenyReason::Unreachable,
                None,
            );
            return;
        };
        let Ok(token) = proto::random_token() else {
            return;
        };
        let Ok(host_eph) = StaticKey::generate() else {
            return;
        };
        let Ok(key) = host_eph.session(&request.eph) else {
            self.deny(
                &request.guest_id,
                &world_id,
                proto::DenyReason::Unreachable,
                guest.lan,
            );
            return;
        };
        let session = self.session_id();
        let accepted = JoinAccepted {
            v: VERSION,
            msg_id: proto::random_id().unwrap_or_default(),
            ts: proto::now_ms(),
            world_id: world_id.clone(),
            host_id: self.id.clone(),
            host_eph: host_eph.public(),
            token: token.clone(),
            session,
        };
        let Ok(envelope) = self.key.seal(
            &self.id,
            &request.guest_id,
            &guest.value.key,
            &Signal::JoinAccepted(accepted),
        ) else {
            return;
        };
        self.send_envelope(&request.guest_id, &envelope, guest.lan);
        self.host_pending.insert(
            session,
            HostPending {
                guest_id: request.guest_id.clone(),
                key,
                token,
                pipe: None,
                seen: Instant::now(),
            },
        );
        self.live.insert(
            session,
            Live {
                peer_name: request.guest_name.clone(),
                hosting: true,
                connected: false,
                commands: None,
                seen: Instant::now(),
            },
        );
        let _ = self.events.send(Event::ServerPipeNeeded {
            session,
            guest: request.guest_name,
        });
        self.notice = Some("A nearby player is joining…".into());
        self.dirty = true;
    }

    fn deny(
        &self,
        guest_id: &str,
        world_id: &str,
        reason: proto::DenyReason,
        lan: Option<SocketAddr>,
    ) {
        let Some(guest) = self.people.get(guest_id) else {
            return;
        };
        let denied = JoinDenied {
            v: VERSION,
            msg_id: proto::random_id().unwrap_or_default(),
            ts: proto::now_ms(),
            world_id: world_id.to_owned(),
            reason,
        };
        if let Ok(envelope) = self.key.seal(
            &self.id,
            guest_id,
            &guest.value.key,
            &Signal::JoinDenied(denied),
        ) {
            self.send_envelope(guest_id, &envelope, lan.or(guest.lan));
        }
    }

    fn provide_pipe(&mut self, session: u64, pipe: horizon_protocol::connection::Pipe) {
        if let Some(live) = self.live.get(&session)
            && let Some(tx) = &live.commands
        {
            let _ = tx.send(SessionCommand::Pipe(pipe));
            return;
        }
        if let Some(pending) = self.host_pending.get_mut(&session) {
            pending.pipe = Some(pipe);
        }
    }

    fn handle_join_request(&mut self, request: JoinRequest) {
        if !self.seen.insert(&request.msg_id) {
            return;
        }
        if request.v != VERSION
            || !proto::valid_id(&request.guest_id)
            || request.guest_name.trim().is_empty()
        {
            return;
        }
        let (world_id, world_name, full) = match &self.sharing {
            Some(sharing) => (
                sharing.presence.id.clone(),
                sharing.presence.name.clone(),
                sharing.players >= sharing.max,
            ),
            None => {
                self.deny(
                    &request.guest_id,
                    &request.world_id,
                    proto::DenyReason::Denied,
                    None,
                );
                return;
            }
        };
        if request.world_id != world_id {
            return;
        }
        let token_ok = request.token.as_ref().is_some_and(|token| {
            self.issued
                .get(token)
                .is_some_and(|(world, _)| world == &world_id)
        });
        if let Some(token) = &request.token
            && token_ok
        {
            self.issued.remove(token);
        }
        if self.config.policy == Policy::InviteOnly && !token_ok {
            self.deny(
                &request.guest_id,
                &world_id,
                proto::DenyReason::InviteOnly,
                None,
            );
            return;
        }
        if full && !token_ok {
            self.deny(&request.guest_id, &world_id, proto::DenyReason::Full, None);
            return;
        }
        let now = Instant::now();
        let times = self
            .request_times
            .entry(request.guest_id.clone())
            .or_default();
        times.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if times.len() >= MAX_REQUESTS_PER_MINUTE {
            return;
        }
        times.push_back(now);
        if self.config.policy == Policy::Open || token_ok {
            self.accept_request_data(request, world_name);
            return;
        }
        self.requests
            .retain(|r| r.request.guest_id != request.guest_id);
        self.requests.push(RequestPending {
            request: request.clone(),
            world_name,
            seen: now,
        });
        let _ = self.events.send(Event::Request(request));
        self.dirty = true;
    }

    fn handle_join_accepted(&mut self, accepted: JoinAccepted) {
        let Some(pending) = self.guest.take() else {
            return;
        };
        if pending.host_id != accepted.host_id || pending.world_id != accepted.world_id {
            self.guest = Some(pending);
            return;
        }
        let Ok(key) = pending.eph.session(&accepted.host_eph) else {
            self.notify("The host's session key did not match");
            return;
        };
        let peer = match Peer::offer(&self.config) {
            Ok(peer) => peer,
            Err(e) => {
                self.notify(&format!("WebRTC did not start: {e}"));
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        match session::guest(
            peer,
            key.clone(),
            accepted.token.clone(),
            accepted.session,
            self.id.clone(),
            accepted.host_id.clone(),
            self.config.connect_timeout,
            rx,
            self.session_tx.clone(),
        ) {
            Ok(_) => {}
            Err(e) => {
                self.notify(&format!("Could not start the session: {e}"));
                return;
            }
        }
        self.guest_keys.insert(accepted.session, key);
        self.live.insert(
            accepted.session,
            Live {
                peer_name: pending.host_name,
                hosting: false,
                connected: false,
                commands: Some(tx),
                seen: Instant::now(),
            },
        );
        self.notice = Some("Connecting…".into());
        self.dirty = true;
    }

    fn handle_offer(&mut self, offer: proto::Offer) {
        let Some(pending) = self.host_pending.remove(&offer.session) else {
            return;
        };
        let peer = match Peer::answer(&self.config, offer.sdp) {
            Ok(peer) => peer,
            Err(e) => {
                self.notify(&format!("WebRTC did not answer: {e}"));
                self.finish(offer.session);
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        match session::host(
            peer,
            pending.key.clone(),
            pending.token.clone(),
            offer.session,
            self.id.clone(),
            pending.guest_id.clone(),
            self.config.connect_timeout,
            rx,
            self.session_tx.clone(),
        ) {
            Ok(_) => {}
            Err(e) => {
                self.notify(&format!("Could not start the session: {e}"));
                self.finish(offer.session);
                return;
            }
        }
        if let Some(pipe) = pending.pipe {
            let _ = tx.send(SessionCommand::Pipe(pipe));
        }
        if let Some(live) = self.live.get_mut(&offer.session) {
            live.commands = Some(tx);
        }
        self.notice = Some("Connecting the near player…".into());
        self.dirty = true;
    }

    fn handle_answer(&mut self, answer: proto::Answer) {
        if let Some(live) = self.live.get(&answer.session)
            && let Some(tx) = &live.commands
        {
            let _ = tx.send(SessionCommand::Answer(answer.sdp));
        }
    }

    fn finish(&mut self, session: u64) {
        self.guest_keys.remove(&session);
        self.host_pending.remove(&session);
        self.live.remove(&session);
        self.dirty = true;
    }

    fn handle_signal(&mut self, signal: Signal, from: String) {
        match signal {
            Signal::JoinRequest(request) => {
                if request.guest_id == from {
                    self.handle_join_request(request);
                }
            }
            Signal::JoinAccepted(accepted) => {
                if accepted.host_id == from {
                    self.handle_join_accepted(accepted);
                }
            }
            Signal::Offer(offer) => {
                let allowed = self
                    .host_pending
                    .get(&offer.session)
                    .is_some_and(|pending| pending.guest_id == from);
                if allowed {
                    self.handle_offer(offer);
                }
            }
            Signal::Answer(answer) => self.handle_answer(answer),
            Signal::JoinDenied(denied) => {
                let matches = self
                    .guest
                    .as_ref()
                    .is_some_and(|pending| pending.host_id == from);
                if matches {
                    self.guest = None;
                    self.notice = Some(format!("Join denied: {:?}", denied.reason));
                    self.dirty = true;
                }
            }
            Signal::Invite(invite) => {
                if invite.host_id != from {
                    return;
                }
                self.invites.retain(|i| i.invite.msg_id != invite.msg_id);
                self.invites.push(InviteSeen {
                    invite: invite.clone(),
                    seen: Instant::now(),
                });
                let _ = self.events.send(Event::Invite(invite));
                self.dirty = true;
            }
            Signal::Bye(bye) => {
                // A peer may only remove itself: a signed envelope still does
                // not let anyone else evict another player's world.
                if bye.id != from {
                    return;
                }
                self.worlds.remove(&bye.id);
                self.people.remove(&bye.id);
                self.dirty = true;
            }
        }
    }

    fn handle_envelope(&mut self, envelope: Envelope) {
        if envelope.to != self.id || envelope.v != VERSION {
            return;
        }
        let signal = if !envelope.eph.is_empty() {
            self.key.open(&envelope)
        } else if let Some(session) = envelope.session {
            if let Some(pending) = self.host_pending.get(&session) {
                pending.key.open(&envelope)
            } else if let Some(key) = self.guest_keys.get(&session) {
                key.open(&envelope)
            } else {
                return;
            }
        } else {
            return;
        };
        if let Ok(signal) = signal {
            self.handle_signal(signal, envelope.from);
        }
    }

    fn handle_presence(&mut self, mut presence: Presence, lan: Option<SocketAddr>) {
        if !presence.sanitize() || presence.id == self.id {
            return;
        }
        // A retained broker record outlives a crashed host; its timestamp
        // tells us not to show it again.
        let stale = presence.ts != 0
            && proto::now_ms().saturating_sub(presence.ts) > PRESENCE_TTL.as_millis() as u64 * 3;
        if stale {
            return;
        }
        let now = Instant::now();
        match presence.kind {
            PresenceKind::World => {
                let Some(host_id) = presence.host_id.clone() else {
                    return;
                };
                let Some(world_id) = presence.world_id.clone() else {
                    return;
                };
                if host_id == self.id {
                    return;
                }
                match self.worlds.get_mut(&world_id) {
                    Some(seen) => {
                        if lan.is_some() {
                            seen.lan = lan;
                        }
                        seen.value = presence;
                        seen.seen = now;
                    }
                    None => {
                        self.worlds.insert(
                            world_id,
                            Seen {
                                value: presence,
                                lan,
                                seen: now,
                            },
                        );
                    }
                }
            }
            PresenceKind::Player => match self.people.get_mut(&presence.id) {
                Some(seen) => {
                    if lan.is_some() {
                        seen.lan = lan;
                    }
                    seen.value = presence;
                    seen.seen = now;
                }
                None => {
                    self.people.insert(
                        presence.id.clone(),
                        Seen {
                            value: presence,
                            lan,
                            seen: now,
                        },
                    );
                }
            },
        }
        self.dirty = true;
    }

    fn drain_lan(&mut self) {
        let Some(rx) = self.lan_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(LanIncoming::Presence(presence, from)) => {
                    self.handle_presence(presence, Some(from))
                }
                Ok(LanIncoming::Signal(bytes, _)) => {
                    if let Ok(envelope) = serde_json::from_slice::<Envelope>(&bytes) {
                        self.handle_envelope(envelope);
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.lan_rx = Some(rx);
                    return;
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn drain_brokers(&mut self) {
        loop {
            match self.broker_rx.try_recv() {
                Ok(BrokerEvent::Connected(url)) => {
                    if let Some(link) = self.links.iter_mut().find(|l| l.url == url) {
                        link.state = "online".into();
                    }
                    self.publish_presence();
                    self.notice = None;
                    self.dirty = true;
                }
                Ok(BrokerEvent::Disconnected(url, reason)) => {
                    if let Some(link) = self.links.iter_mut().find(|l| l.url == url) {
                        link.state = format!("offline: {reason}");
                    }
                    self.dirty = true;
                }
                Ok(BrokerEvent::Publish { topic, payload }) => {
                    self.handle_broker_publish(&topic, &payload);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn handle_broker_publish(&mut self, topic: &str, payload: &[u8]) {
        let Some(room) = &self.room else {
            return;
        };
        if !topic.starts_with(&format!("horizon/nearby/v1/{room}/")) {
            return;
        }
        let tail = topic.rsplit('/').next().unwrap_or_default();
        if topic.contains("/presence/") {
            if payload.is_empty() {
                self.worlds.remove(tail);
                self.people.remove(tail);
                self.dirty = true;
                return;
            }
            if let Ok(presence) = serde_json::from_slice::<Presence>(payload) {
                self.handle_presence(presence, None);
            }
        } else if topic.contains("/inbox/")
            && let Ok(envelope) = serde_json::from_slice::<Envelope>(payload)
        {
            self.handle_envelope(envelope);
        }
    }

    fn drain_sessions(&mut self) {
        loop {
            match self.session_rx.try_recv() {
                Ok(SessionEvent::Signal(envelope)) => {
                    let lan = self
                        .people
                        .get(&envelope.to)
                        .and_then(|p| p.lan)
                        .or_else(|| {
                            self.worlds
                                .values()
                                .find(|w| w.value.host_id.as_deref() == Some(&envelope.to))
                                .and_then(|w| w.lan)
                        });
                    self.send_envelope(&envelope.to, &envelope, lan);
                }
                Ok(SessionEvent::Ready { session, pipe }) => {
                    let host = match self.live.get_mut(&session) {
                        Some(live) => {
                            live.connected = true;
                            live.peer_name.clone()
                        }
                        None => continue,
                    };
                    self.notice = None;
                    self.dirty = true;
                    let _ = self.events.send(Event::GuestReady {
                        session,
                        pipe,
                        host,
                    });
                }
                Ok(SessionEvent::Connected { session }) => {
                    if let Some(live) = self.live.get_mut(&session) {
                        live.connected = true;
                    }
                    self.dirty = true;
                }
                Ok(SessionEvent::Closed { session, reason }) => {
                    self.finish(session);
                    let _ = self.events.send(Event::SessionClosed { session, reason });
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn session_id(&mut self) -> u64 {
        self.next_session = self.next_session.wrapping_add(1).max(1);
        self.next_session
    }

    fn expire(&mut self) {
        let now = Instant::now();
        self.worlds
            .retain(|_, seen| now.duration_since(seen.seen) < PRESENCE_TTL);
        self.people
            .retain(|_, seen| now.duration_since(seen.seen) < PRESENCE_TTL);
        self.requests
            .retain(|request| now.duration_since(request.seen) < REQUEST_TTL);
        self.invites
            .retain(|invite| now.duration_since(invite.seen) < INVITE_TTL);
        self.issued
            .retain(|_, (_, at)| now.duration_since(*at) < INVITE_TTL);
        self.request_times.retain(|_, times| {
            times.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
            !times.is_empty()
        });
        self.host_pending
            .retain(|_, pending| now.duration_since(pending.seen) < SESSION_TTL);
        let live_sessions: HashSet<u64> = self.live.keys().copied().collect();
        self.guest_keys
            .retain(|session, _| live_sessions.contains(session));
        self.live
            .retain(|_, live| live.connected || now.duration_since(live.seen) < SESSION_TTL);
        let guest_timed_out = self
            .guest
            .as_ref()
            .is_some_and(|pending| now.duration_since(pending.seen) >= SESSION_TTL);
        if guest_timed_out {
            self.guest = None;
            self.notice = Some("The host did not answer".into());
        }
        self.seen.expire();
        self.dirty = true;
    }

    fn write_snapshot(&mut self) {
        let snapshot = self.make_snapshot();
        if let Ok(mut guard) = horizon_os::sync::lock(&self.snapshot) {
            *guard = snapshot;
        }
        let _ = self.events.send(Event::SnapshotChanged);
        self.dirty = false;
    }

    fn make_snapshot(&self) -> Snapshot {
        let mut worlds: Vec<WorldRow> = self
            .worlds
            .values()
            .filter_map(|seen| {
                let p = &seen.value;
                let host_id = p.host_id.clone()?;
                let host_name = self
                    .people
                    .get(&host_id)
                    .map(|h| h.value.name.clone())
                    .unwrap_or_else(|| p.name.clone());
                Some(WorldRow {
                    world_id: p.world_id.clone()?,
                    name: p.name.clone(),
                    host_id,
                    host_name,
                    players: p.players.unwrap_or(1),
                    max: p.max.unwrap_or(8),
                    policy: p.policy.unwrap_or_default(),
                    compatible: compatible(self, p),
                    direct: seen.lan.is_some(),
                })
            })
            .collect();
        worlds.sort_by_key(|world| world.name.to_lowercase());
        let mut people: Vec<PersonRow> = self
            .people
            .values()
            .filter(|seen| seen.value.id != self.id)
            .map(|seen| PersonRow {
                id: seen.value.id.clone(),
                name: seen.value.name.clone(),
                status: seen.value.status,
                compatible: compatible(self, &seen.value),
                direct: seen.lan.is_some(),
            })
            .collect();
        people.sort_by_key(|person| person.name.to_lowercase());
        let mut requests: Vec<RequestRow> = self
            .requests
            .iter()
            .map(|pending| RequestRow {
                id: pending.request.msg_id.clone(),
                guest_name: pending.request.guest_name.clone(),
                world_name: pending.world_name.clone(),
            })
            .collect();
        requests.sort_by(|a, b| a.guest_name.cmp(&b.guest_name));
        let invites: Vec<InviteRow> = self
            .invites
            .iter()
            .map(|seen| InviteRow {
                id: seen.invite.msg_id.clone(),
                host_name: seen.invite.host_name.clone(),
                world_name: seen.invite.world_name.clone(),
            })
            .collect();
        let mut sessions: Vec<SessionRow> = self
            .live
            .iter()
            .map(|(session, live)| SessionRow {
                session: *session,
                peer: live.peer_name.clone(),
                hosting: live.hosting,
                connected: live.connected,
            })
            .collect();
        sessions.sort_by_key(|s| s.session);
        let sharing = self.sharing.as_ref().map(|sharing| SharedWorld {
            world_id: sharing.presence.id.clone(),
            name: sharing.presence.name.clone(),
            players: sharing.players,
            max: sharing.max,
        });
        let brokers = self
            .links
            .iter()
            .map(|link| BrokerStatus {
                url: link.url.clone(),
                state: link.state.clone(),
            })
            .collect();
        Snapshot {
            enabled: true,
            room: self.room.clone(),
            policy: self.config.policy,
            brokers,
            sharing,
            worlds,
            people,
            requests,
            invites,
            sessions,
            notice: self.notice.clone(),
        }
    }

    fn set_room(&mut self, code: Option<String>) {
        self.stop_brokers();
        self.worlds.clear();
        self.people.clear();
        self.requests.clear();
        self.invites.clear();
        let custom = code.as_deref().and_then(room::custom);
        self.room = if let Some(custom) = custom {
            Some(custom)
        } else {
            room::automatic(probe_public_ip(&self.config.stun))
        };
        self.start_brokers();
        self.publish_presence();
        self.notice = if self.room.is_some() {
            Some("Looking for nearby players…".into())
        } else {
            Some("No nearby room — local network only".into())
        };
        self.dirty = true;
    }

    fn notify(&mut self, message: &str) {
        self.notice = Some(message.to_owned());
        let _ = self.events.send(Event::Notice(message.to_owned()));
        self.dirty = true;
    }

    fn shutdown(&mut self) {
        self.stop_share();
        if let Some(room) = &self.room {
            for id in [self.id.clone()] {
                let topic = format!("horizon/nearby/v1/{room}/presence/{id}");
                for link in &self.links {
                    let _ = link.commands.send(BrokerCommand::Publish {
                        topic: topic.clone(),
                        payload: Vec::new(),
                        retain: true,
                    });
                }
            }
        }
        self.stop_brokers();
        self.lan = None;
        self.lan_rx = None;
        self.dirty = true;
        self.write_snapshot();
    }
}

fn compatible(inner: &Inner, presence: &Presence) -> bool {
    presence.proto == inner.config.proto && presence.game == inner.config.game
}

#[cfg(not(target_arch = "wasm32"))]
fn probe_public_ip(stun: &[String]) -> Option<std::net::IpAddr> {
    horizon_friends::stun::public_ip(stun, Duration::from_secs(2))
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
fn probe_public_ip(stun: &[String]) -> Option<std::net::IpAddr> {
    let ice: Vec<String> = stun
        .iter()
        .filter(|server| !server.starts_with("turn:") && !server.starts_with("turns:"))
        .map(|server| {
            if server.starts_with("stun:") {
                server.clone()
            } else {
                format!("stun:{server}")
            }
        })
        .collect();
    horizon_os::net::public_address(&ice, Duration::from_secs(6)).ok()
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn probe_public_ip(_stun: &[String]) -> Option<std::net::IpAddr> {
    None
}
