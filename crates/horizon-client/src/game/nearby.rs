//! Play Nearby, game side: the service (`horizon-nearby`), the pause menu's
//! screen, auto-sharing the open world, and the pipes that put a WebRTC
//! guest on the integrated server.
use super::{Game, Screen};
use horizon_client::{
    Result,
    hud::{HudBatch, rgba},
    lan::MAX_PLAYERS,
    nearby_screen::{self, Action, NearbyView},
    text::{ClickEvent, Style, Text},
    title::Mode,
    toasts::SystemToast,
};
use horizon_nearby::{
    Command as NearbyCommand, Event as NearbyEvent, Policy, Service, proto::PlayerStatus,
};
use horizon_os::time::{Duration, Instant};
use horizon_protocol::{GAME_VERSION, PROTOCOL_VERSION};

pub const HELP: &[&str] = &[
    "nearby status                 the room, brokers, worlds, people, requests, sessions",
    "nearby share | stop           share the open world, or stop",
    "nearby join <world-id>        ask to join a nearby world",
    "nearby accept|deny <id>       answer a join request",
    "nearby invite <player-id>     invite a nearby player",
    "nearby accept-invite <id>     take an invite",
    "nearby code <code>            use a custom room code",
];

pub(super) struct Nearby {
    service: Service,
    /// The share the service was last told about (world, players, max).
    shared: Option<(String, usize, usize)>,
    last_status: Option<PlayerStatus>,
    hosted: usize,
    copied: Option<Instant>,
    auto_share: bool,
    /// Whether the player asked to be dropped into the first open nearby world
    /// they can see, instead of picking one from the list.
    auto_join: bool,
    toast: Option<SystemToast>,
}

impl Game<'_> {
    /// Start the service once the options exist; a dedicated server and the
    /// server thread itself keep none.
    fn ensure_nearby(&mut self) {
        if self.nearby.is_some() || self.serving.is_some() || self.options.dedicated.is_some() {
            return;
        }
        let dir = horizon_os::env::current_dir().unwrap_or_default();
        let preference = |key: &str| horizon_client::ui_theme::preference(&dir, key);
        let env_list = |name: &str| {
            std::env::var(name).ok().map(|list| {
                list.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        };
        let env_flag = |name: &str, default: bool| {
            std::env::var(name).map_or(default, |v| v != "0" && v != "false" && !v.is_empty())
        };
        let enabled = env_flag("HORIZON_NEARBY", true)
            && preference("nearbyEnabled")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off"));
        if !enabled {
            self.nearby = Some(Nearby {
                service: Service::disabled(),
                shared: None,
                last_status: None,
                hosted: 0,
                copied: None,
                auto_share: false,
                auto_join: false,
                toast: None,
            });
            return;
        }
        let brokers = env_list("HORIZON_NEARBY_BROKERS")
            .or_else(|| {
                preference("nearbyBrokers").map(|list| {
                    list.split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
            })
            .filter(|list: &Vec<String>| !list.is_empty())
            .unwrap_or_else(horizon_nearby::service::default_brokers);
        let stun = env_list("HORIZON_NEARBY_STUN")
            .or_else(|| {
                preference("nearbyStun").map(|list| {
                    list.split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
            })
            .filter(|list: &Vec<String>| !list.is_empty())
            .unwrap_or_else(horizon_nearby::service::default_stun);
        let room = std::env::var("HORIZON_NEARBY_ROOM")
            .ok()
            .or_else(|| preference("nearbyRoom"))
            .filter(|code| !code.trim().is_empty());
        let policy = std::env::var("HORIZON_NEARBY_POLICY")
            .ok()
            .or_else(|| preference("nearbyPolicy"))
            .map(|value| match value.to_ascii_lowercase().as_str() {
                "open" | "anyone" => Policy::Open,
                "invite" | "invite_only" | "invite-only" => Policy::InviteOnly,
                _ => Policy::Ask,
            })
            .unwrap_or_default();
        let auto_share = env_flag("HORIZON_NEARBY_SHARE", true)
            && preference("nearbyShare")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off"));
        let auto_join = env_flag("HORIZON_NEARBY_AUTOJOIN", true)
            && preference("nearbyAutoJoin")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off"));
        let config = horizon_nearby::Config {
            enabled: true,
            player_name: self.options.player_name(),
            proto: PROTOCOL_VERSION,
            game: GAME_VERSION.to_owned(),
            policy,
            brokers,
            room,
            stun,
            lan: env_flag("HORIZON_NEARBY_LAN", true),
            ice_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(30),
            loopback: env_flag("HORIZON_NEARBY_LOOPBACK", false),
        };
        self.nearby = Some(Nearby {
            service: Service::start(config),
            shared: None,
            last_status: None,
            hosted: 0,
            copied: None,
            auto_share,
            auto_join,
            toast: None,
        });
    }

    /// Once a frame, paused or not: events, auto-sharing, the title's actions.
    pub(super) fn tick_nearby(&mut self) {
        self.ensure_nearby();
        let events = self
            .nearby
            .as_ref()
            .map(|nearby| nearby.service.poll())
            .unwrap_or_default();
        for event in events {
            self.nearby_event(event);
        }
        self.nearby_auto_share();
        self.nearby_refresh_hosted();
        self.arm_nearby_auto_join();
    }

    fn nearby_event(&mut self, event: NearbyEvent) {
        match event {
            NearbyEvent::SnapshotChanged => {}
            NearbyEvent::Request(request) => {
                self.nearby_alert(format!("{} wants to join your world.", request.guest_name));
            }
            NearbyEvent::Invite(invite) => {
                self.nearby_alert(format!(
                    "{} invited you to {}.",
                    invite.host_name, invite.world_name
                ));
            }
            NearbyEvent::ServerPipeNeeded { session, guest } => {
                let pipe = self.integrated.as_ref().and_then(|server| {
                    server
                        .call(move |g| g.lan.as_mut()?.hub.lan.connect_local().ok())
                        .flatten()
                });
                match pipe {
                    Some(pipe) => {
                        if let Some(nearby) = &self.nearby {
                            nearby
                                .service
                                .command(NearbyCommand::ProvideServerPipe { session, pipe });
                        }
                    }
                    None => self.nearby_notice(format!(
                        "{guest} was accepted, but the world could not take the connection."
                    )),
                }
            }
            NearbyEvent::GuestReady {
                session: _,
                pipe,
                host,
            } => {
                let name = self.options.player_name();
                if let Err(e) = self.connect_local(pipe, &name) {
                    self.nearby_notice(format!("Could not join {host}: {e}"));
                }
            }
            NearbyEvent::SessionClosed { session: _, reason } => {
                self.nearby_refresh_hosted();
                if let Some(reason) = reason {
                    self.nearby_notice(format!("The nearby session ended: {reason}"));
                }
                // The player is back on their own: look for another world to
                // drop into, if they asked for automatic joining.
                self.arm_nearby_auto_join();
            }
            NearbyEvent::Notice(message) => self.nearby_notice(message),
        }
    }

    /// A toast (in world and on the title) and a clickable chat line.
    fn nearby_alert(&mut self, message: String) {
        if let Some(nearby) = &mut self.nearby {
            nearby.toast = Some(SystemToast::new(
                "Play Nearby".to_owned(),
                Some(message.clone()),
            ));
        }
        if let Some(title) = &mut self.title {
            title.nearby_toast(message.clone());
        }
        let action = Text::styled(
            "[Open Play Nearby]",
            Style {
                color: Some(rgba(85, 255, 85, 255)),
                underlined: Some(true),
                click: Some(ClickEvent::RunCommand("nearby".to_owned())),
                ..Style::default()
            },
        );
        self.system_message(Text::literal(format!("{message} ")).append(action));
    }

    fn nearby_notice(&mut self, message: String) {
        if let Some(nearby) = &mut self.nearby {
            nearby.toast = Some(SystemToast::new(
                "Play Nearby".to_owned(),
                Some(message.clone()),
            ));
        }
        self.system_message(Text::literal(message));
    }

    fn nearby_auto_share(&mut self) {
        let in_world = self.remote.is_some()
            && self.integrated.is_some()
            && self.title.is_none()
            && self.serving.is_none();
        let world = self
            .integrated
            .as_ref()
            .map(|server| server.world_name.clone())
            .filter(|name| !name.is_empty());
        let players = self
            .remote_online_names()
            .map(|names| names.len().max(1))
            .unwrap_or(1);
        let Some(nearby) = self.nearby.as_mut() else {
            return;
        };
        let share = in_world && nearby.auto_share && world.is_some();
        if share {
            let name = world.expect("checked");
            if nearby.shared.as_ref() != Some(&(name.clone(), players, MAX_PLAYERS)) {
                nearby.service.command(NearbyCommand::ShareWorld {
                    name: name.clone(),
                    players,
                    max: MAX_PLAYERS,
                });
                nearby.shared = Some((name, players, MAX_PLAYERS));
            }
        } else if nearby.shared.take().is_some() {
            nearby.service.command(NearbyCommand::StopShare);
        }
        let status = if self.remote.is_some() {
            PlayerStatus::Playing
        } else {
            PlayerStatus::Menu
        };
        if nearby.last_status != Some(status) {
            nearby
                .service
                .command(NearbyCommand::SetPlayerStatus(status));
            nearby.last_status = Some(status);
        }
    }

    /// Arms automatic joining, if the player asked for it: the service then
    /// drops them into the first open compatible world it sees, and this is
    /// called again when a session ends so they can move on.
    fn arm_nearby_auto_join(&mut self) {
        let Some(nearby) = &self.nearby else {
            return;
        };
        if !nearby.auto_join {
            return;
        }
        // Only from the title: the player is between worlds and has nowhere of
        // their own to lose. Joining a world also needs the title, since
        // `connect_local` replaces the current screen.
        if self.title.is_none() || self.serving.is_some() {
            return;
        }
        nearby.service.command(NearbyCommand::AutoJoin);
    }

    fn nearby_refresh_hosted(&mut self) {
        let Some(nearby) = &self.nearby else {
            return;
        };
        let hosted = nearby
            .service
            .snapshot()
            .sessions
            .iter()
            .filter(|session| session.hosting && session.connected)
            .count();
        let changed = hosted != nearby.hosted;
        if changed && let Some(nearby) = &mut self.nearby {
            nearby.hosted = hosted;
        }
        if let Some(server) = &self.integrated {
            server.set_nearby_guests(hosted);
        }
    }

    pub(super) fn nearby_view(&self) -> NearbyView {
        let snapshot = self
            .nearby
            .as_ref()
            .map(|nearby| nearby.service.snapshot())
            .unwrap_or_default();
        NearbyView {
            snapshot,
            in_world: self.integrated.is_some() && self.title.is_none(),
            copied: self
                .nearby
                .as_ref()
                .and_then(|nearby| nearby.copied)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2)),
        }
    }

    /// Apply a button from the Play Nearby screen.
    pub(super) fn nearby_action(&mut self, action: Action) {
        let snapshot = self
            .nearby
            .as_ref()
            .map(|nearby| nearby.service.snapshot())
            .unwrap_or_default();
        match action {
            Action::Share => {
                if let Some(nearby) = &mut self.nearby {
                    nearby.auto_share = true;
                    nearby.shared = None;
                }
                self.save_nearby("nearbyShare", Some("true"));
                self.nearby_auto_share();
            }
            Action::StopShare => {
                if let Some(nearby) = &mut self.nearby {
                    nearby.auto_share = false;
                    nearby.shared = None;
                    nearby.service.command(NearbyCommand::StopShare);
                }
                self.save_nearby("nearbyShare", Some("false"));
            }
            Action::CyclePolicy => {
                let next = match snapshot.policy {
                    Policy::Ask => Policy::Open,
                    Policy::Open => Policy::InviteOnly,
                    Policy::InviteOnly => Policy::Ask,
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::SetPolicy(next));
                }
                let value = match next {
                    Policy::Ask => "ask",
                    Policy::Open => "open",
                    Policy::InviteOnly => "invite",
                };
                self.save_nearby("nearbyPolicy", Some(value));
            }
            Action::CopyRoom => {
                if let Some(room) = &snapshot.room {
                    use horizon_client::clipboard::Clipboard;
                    self.clipboard.set(&room.to_uppercase());
                    if let Some(nearby) = &mut self.nearby {
                        nearby.copied = Some(Instant::now());
                    }
                }
            }
            Action::Join(world_id) => {
                if let Some(nearby) = &self.nearby {
                    nearby
                        .service
                        .command(NearbyCommand::RequestJoin { world_id });
                }
            }
            Action::Accept(request_id) => {
                if let Some(nearby) = &self.nearby {
                    nearby
                        .service
                        .command(NearbyCommand::AcceptRequest { request_id });
                }
            }
            Action::Deny(request_id) => {
                if let Some(nearby) = &self.nearby {
                    nearby
                        .service
                        .command(NearbyCommand::DenyRequest { request_id });
                }
            }
            Action::Invite(player_id) => {
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::Invite { player_id });
                }
            }
            Action::AcceptInvite(invite_id) => {
                if let Some(nearby) = &self.nearby {
                    nearby
                        .service
                        .command(NearbyCommand::AcceptInvite { invite_id });
                }
            }
            Action::Done => self.screen = Screen::Paused,
        }
    }

    /// A Play Nearby button on the title: Done closes it, the rest apply.
    pub(super) fn title_nearby_action(&mut self, action: Action) {
        if action == Action::Done {
            if let Some(title) = &mut self.title {
                title.mode = Mode::Main;
            }
            return;
        }
        self.nearby_action(action);
    }

    fn save_nearby(&self, key: &str, value: Option<&str>) {
        let dir = horizon_os::env::current_dir().unwrap_or_default();
        horizon_client::ui_theme::set_preference(&dir, key, value);
    }

    /// `/nearby`, from a clicked chat line: open the screen.
    pub(super) fn nearby_command(&mut self, command: &str) {
        if command.trim() == "nearby" {
            if self.title.is_some() {
                if let Some(title) = &mut self.title {
                    title.mode = Mode::Nearby;
                }
            } else {
                self.screen = Screen::Nearby;
            }
        }
    }

    pub(super) fn draw_nearby_screen(&mut self, batch: &mut HudBatch) {
        let view = self.nearby_view();
        nearby_screen::draw(batch, &self.gui.atlas, self.cursor, &view, true);
    }

    pub(super) fn nearby_screen_click(&mut self) {
        let view = self.nearby_view();
        let Some(action) =
            nearby_screen::click(&self.gui.atlas, self.gui_size(), self.cursor, &view)
        else {
            return;
        };
        self.audio.play_button_click();
        self.nearby_action(action);
    }

    /// The nearby toast shares the `ToastManager`'s slots with the rest.
    pub(super) fn draw_nearby_toast(&mut self, batch: &mut HudBatch) {
        let title = self.title.as_ref().is_some_and(|title| title.has_toast());
        let used = self.toast_slots_used() | u32::from(title);
        let multiplier = f64::from(self.settings.notification_display_time);
        let hidden = self.ui.hide_gui;
        let Some(nearby) = self.nearby.as_mut() else {
            return;
        };
        let sound = super::narrator::draw_toast(
            &mut nearby.toast,
            used,
            multiplier,
            &self.gui.atlas,
            batch,
            hidden,
        );
        if let Some(show) = sound {
            let event = if show { "ui.toast.in" } else { "ui.toast.out" };
            self.audio
                .play(&format!("minecraft:{event}"), None, 1.0, 1.0);
        }
    }

    /// `horizon-ctl nearby ...`.
    pub(super) fn nearby_control(&mut self, words: &[&str]) -> Result<Option<serde_json::Value>> {
        use serde_json::json;
        if words.first().copied() != Some("nearby") {
            return Ok(None);
        }
        self.tick_nearby();
        match words.get(1).copied() {
            Some("status") | None => {
                let view = self.nearby_view();
                let snapshot = &view.snapshot;
                Ok(Some(json!({
                    "enabled": snapshot.enabled,
                    "room": snapshot.room,
                    "policy": format!("{:?}", snapshot.policy).to_lowercase(),
                    "notice": snapshot.notice,
                    "sharing": snapshot.sharing.as_ref().map(|world| json!({
                        "world_id": world.world_id,
                        "name": world.name,
                        "players": world.players,
                        "max": world.max,
                    })),
                    "worlds": snapshot.worlds.iter().map(|world| json!({
                        "world_id": world.world_id,
                        "name": world.name,
                        "host": world.host_name,
                        "players": world.players,
                        "max": world.max,
                        "compatible": world.compatible,
                        "direct": world.direct,
                    })).collect::<Vec<_>>(),
                    "people": snapshot.people.iter().map(|person| json!({
                        "id": person.id,
                        "name": person.name,
                        "status": format!("{:?}", person.status).to_lowercase(),
                        "direct": person.direct,
                    })).collect::<Vec<_>>(),
                    "requests": snapshot.requests.iter().map(|request| json!({
                        "id": request.id,
                        "guest": request.guest_name,
                        "world": request.world_name,
                    })).collect::<Vec<_>>(),
                    "invites": snapshot.invites.iter().map(|invite| json!({
                        "id": invite.id,
                        "host": invite.host_name,
                        "world": invite.world_name,
                    })).collect::<Vec<_>>(),
                    "sessions": snapshot.sessions.iter().map(|session| json!({
                        "session": session.session,
                        "peer": session.peer,
                        "hosting": session.hosting,
                        "connected": session.connected,
                    })).collect::<Vec<_>>(),
                    "brokers": snapshot.brokers.iter().map(|broker| json!({
                        "url": broker.url,
                        "state": broker.state,
                    })).collect::<Vec<_>>(),
                })))
            }
            Some("share") => {
                if let Some(nearby) = &mut self.nearby {
                    nearby.auto_share = true;
                    nearby.shared = None;
                }
                self.nearby_auto_share();
                Ok(Some(json!({ "ok": true })))
            }
            Some("stop") => {
                if let Some(nearby) = &mut self.nearby {
                    nearby.auto_share = false;
                    nearby.shared = None;
                    nearby.service.command(NearbyCommand::StopShare);
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("join") => {
                let Some(world_id) = words.get(2) else {
                    return Err("nearby join <world-id>".into());
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::RequestJoin {
                        world_id: (*world_id).to_owned(),
                    });
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("accept") => {
                let Some(request_id) = words.get(2) else {
                    return Err("nearby accept <request-id>".into());
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::AcceptRequest {
                        request_id: (*request_id).to_owned(),
                    });
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("deny") => {
                let Some(request_id) = words.get(2) else {
                    return Err("nearby deny <request-id>".into());
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::DenyRequest {
                        request_id: (*request_id).to_owned(),
                    });
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("invite") => {
                let Some(player_id) = words.get(2) else {
                    return Err("nearby invite <player-id>".into());
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::Invite {
                        player_id: (*player_id).to_owned(),
                    });
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("accept-invite") => {
                let Some(invite_id) = words.get(2) else {
                    return Err("nearby accept-invite <invite-id>".into());
                };
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::AcceptInvite {
                        invite_id: (*invite_id).to_owned(),
                    });
                }
                Ok(Some(json!({ "ok": true })))
            }
            Some("code") => {
                let code = words.get(2).map(|code| (*code).to_owned());
                if let Some(nearby) = &self.nearby {
                    nearby.service.command(NearbyCommand::SetRoom(code.clone()));
                }
                self.save_nearby("nearbyRoom", code.as_deref());
                Ok(Some(json!({ "ok": true })))
            }
            _ => Err("nearby status|share|stop|join|accept|deny|invite|accept-invite|code".into()),
        }
    }
}
