//! Play Nearby: the worlds and players on this network, join requests, and
//! the host's share controls. Drawn like a mod screen: the menu background,
//! a centred title, text rows and vanilla buttons.
use crate::{
    hud::{HudAtlas, HudBatch, rgba},
    multiplayer::wrap,
    text::Text,
    ui,
};
use horizon_nearby::{
    Policy, Snapshot,
    service::{InviteRow, PersonRow, RequestRow, WorldRow},
};

pub const TITLE: &str = "Play Nearby";

/// What the screen shows: the service's snapshot plus local UI state.
#[derive(Clone, Debug, Default)]
pub struct NearbyView {
    pub snapshot: Snapshot,
    pub in_world: bool,
    /// The room code was just copied.
    pub copied: bool,
}

impl NearbyView {
    pub fn status(&self) -> String {
        let snap = &self.snapshot;
        if !snap.enabled {
            return "Nearby play is off.".into();
        }
        if let Some(notice) = &snap.notice {
            return notice.clone();
        }
        let online = snap.brokers.iter().filter(|b| b.state == "online").count();
        let mut parts = Vec::new();
        if !snap.worlds.is_empty() {
            parts.push(format!(
                "{} nearby world{}",
                snap.worlds.len(),
                plural(snap.worlds.len())
            ));
        }
        if !snap.people.is_empty() {
            parts.push(format!(
                "{} nearby player{}",
                snap.people.len(),
                plural(snap.people.len())
            ));
        }
        if parts.is_empty() {
            parts.push(if snap.room.is_some() {
                "Looking for nearby worlds…".into()
            } else {
                "Local network only.".into()
            });
        }
        if online > 0 {
            parts.push(format!("{online} broker{} online", plural(online)));
        } else if snap.room.is_some() {
            parts.push("signalling offline".into());
        }
        parts.join(" · ")
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A button on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Share,
    StopShare,
    CyclePolicy,
    CopyRoom,
    Join(String),
    Accept(String),
    Deny(String),
    Invite(String),
    AcceptInvite(String),
    Done,
}

struct Hot {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label: Text,
    action: Action,
    active: bool,
}

fn grey() -> u32 {
    rgba(160, 160, 160, 255)
}
const WHITE: u32 = u32::MAX;

/// The text rows (text, colour, y) and the buttons, at `size`.
fn layout(
    atlas: &HudAtlas,
    (w, h): (f32, f32),
    view: &NearbyView,
) -> (Vec<(String, u32, f32)>, Vec<Hot>) {
    let snap = &view.snapshot;
    let width = (w - 40.0).min(420.0);
    let x0 = ((w - width) / 2.0).floor();
    let x1 = x0 + width;
    let mut lines: Vec<(String, u32, f32)> = Vec::new();
    let mut hots: Vec<Hot> = Vec::new();
    let mut y = 28.0;
    let line = |lines: &mut Vec<(String, u32, f32)>, text: &str, color: u32, y: &mut f32| {
        for text in wrap(text, width, atlas) {
            lines.push((text, color, *y));
            *y += 9.0;
        }
    };
    line(&mut lines, &view.status(), grey(), &mut y);
    y += 2.0;

    let section = |lines: &mut Vec<(String, u32, f32)>, text: &str, y: &mut f32| {
        lines.push((text.to_owned(), WHITE, *y));
        *y += 10.0;
    };

    if view.in_world {
        section(&mut lines, "Your world", &mut y);
        if let Some(sharing) = &snap.sharing {
            let text = format!(
                "{} · {}/{} here · {}",
                sharing.name,
                sharing.players,
                sharing.max,
                snap.policy.label()
            );
            line(&mut lines, &text, WHITE, &mut y);
            let bw = 62.0;
            hots.push(Hot {
                x: x1 - bw,
                y: y - 10.0,
                w: bw,
                h: 12.0,
                label: Text::literal("Stop"),
                action: Action::StopShare,
                active: true,
            });
            hots.push(Hot {
                x: x1 - bw * 2.0 - 4.0,
                y: y - 10.0,
                w: bw,
                h: 12.0,
                label: Text::literal(snap.policy.label()),
                action: Action::CyclePolicy,
                active: true,
            });
        } else {
            line(&mut lines, "Not shared nearby.", grey(), &mut y);
            hots.push(Hot {
                x: x1 - 70.0,
                y: y - 10.0,
                w: 70.0,
                h: 12.0,
                label: Text::literal("Share"),
                action: Action::Share,
                active: true,
            });
        }
        if let Some(room) = &snap.room {
            let text = format!("Code {}", room.to_uppercase());
            line(&mut lines, &text, grey(), &mut y);
            hots.push(Hot {
                x: x1 - 40.0,
                y: y - 10.0,
                w: 40.0,
                h: 12.0,
                label: Text::literal(if view.copied { "Copied" } else { "Copy" }),
                action: Action::CopyRoom,
                active: true,
            });
        }
        y += 3.0;
    }

    if !snap.requests.is_empty() {
        section(&mut lines, "Join requests", &mut y);
        for request in snap.requests.iter().take(3) {
            if y + 11.0 > h - 34.0 {
                break;
            }
            row_text(&mut lines, &request_row(request), x0, x1, &mut y, atlas);
            let mut x = x1 - 44.0;
            hots.push(Hot {
                x,
                y: y - 10.0,
                w: 44.0,
                h: 12.0,
                label: Text::literal("Deny"),
                action: Action::Deny(request.id.clone()),
                active: true,
            });
            x -= 52.0;
            hots.push(Hot {
                x,
                y: y - 10.0,
                w: 48.0,
                h: 12.0,
                label: Text::literal("Accept"),
                action: Action::Accept(request.id.clone()),
                active: true,
            });
            y += 1.0;
        }
        if snap.requests.len() > 3 {
            line(
                &mut lines,
                &format!("and {} more…", snap.requests.len() - 3),
                grey(),
                &mut y,
            );
        }
        y += 3.0;
    }

    if !snap.worlds.is_empty() {
        section(&mut lines, "Nearby worlds", &mut y);
        for world in snap.worlds.iter().take(5) {
            if y + 11.0 > h - 34.0 {
                break;
            }
            let text = world_row(world);
            row_text(&mut lines, &text, x0, x1, &mut y, atlas);
            hots.push(Hot {
                x: x1 - 42.0,
                y: y - 10.0,
                w: 42.0,
                h: 12.0,
                label: Text::literal("Join"),
                action: Action::Join(world.world_id.clone()),
                active: world.compatible,
            });
        }
        if snap.worlds.len() > 5 {
            line(
                &mut lines,
                &format!("and {} more…", snap.worlds.len() - 5),
                grey(),
                &mut y,
            );
        }
        y += 3.0;
    }

    if !snap.people.is_empty() {
        section(&mut lines, "Nearby players", &mut y);
        let can_invite = snap.sharing.is_some();
        for person in snap.people.iter().take(5) {
            if y + 11.0 > h - 34.0 {
                break;
            }
            let text = person_row(person);
            row_text(&mut lines, &text, x0, x1, &mut y, atlas);
            hots.push(Hot {
                x: x1 - 46.0,
                y: y - 10.0,
                w: 46.0,
                h: 12.0,
                label: Text::literal("Invite"),
                action: Action::Invite(person.id.clone()),
                active: can_invite && person.status == horizon_nearby::proto::PlayerStatus::Menu,
            });
        }
        if snap.people.len() > 5 {
            line(
                &mut lines,
                &format!("and {} more…", snap.people.len() - 5),
                grey(),
                &mut y,
            );
        }
        y += 3.0;
    }

    if !snap.invites.is_empty() {
        section(&mut lines, "Invites", &mut y);
        for invite in snap.invites.iter().take(3) {
            if y + 11.0 > h - 34.0 {
                break;
            }
            let text = invite_row(invite);
            row_text(&mut lines, &text, x0, x1, &mut y, atlas);
            hots.push(Hot {
                x: x1 - 42.0,
                y: y - 10.0,
                w: 42.0,
                h: 12.0,
                label: Text::literal("Join"),
                action: Action::AcceptInvite(invite.id.clone()),
                active: true,
            });
        }
        y += 3.0;
    }

    if !snap.sessions.is_empty() {
        section(&mut lines, "Here now", &mut y);
        for session in snap.sessions.iter().take(4) {
            if y + 11.0 > h - 34.0 {
                break;
            }
            let text = format!(
                "{} · {} · {}",
                session.peer,
                if session.hosting { "host" } else { "guest" },
                if session.connected {
                    "connected"
                } else {
                    "connecting"
                }
            );
            row_text(&mut lines, &text, x0, x1, &mut y, atlas);
        }
        y += 3.0;
    }

    if !view.in_world && snap.worlds.is_empty() && snap.people.is_empty() {
        line(
            &mut lines,
            "Open a singleplayer world to share it here, or ask to join a world above.",
            grey(),
            &mut y,
        );
    }
    let _ = y;
    hots.push(Hot {
        x: (w / 2.0).floor() - 100.0,
        y: h - 27.0,
        w: 200.0,
        h: 20.0,
        label: Text::translate("gui.done", &[]),
        action: Action::Done,
        active: true,
    });
    (lines, hots)
}

fn row_text(
    lines: &mut Vec<(String, u32, f32)>,
    text: &str,
    x0: f32,
    x1: f32,
    y: &mut f32,
    atlas: &HudAtlas,
) {
    // Leave the right side to the row's action buttons.
    let max = (x1 - x0 - 70.0).max(40.0);
    let mut fitted = text.to_owned();
    while !fitted.is_empty() && HudBatch::text_width(atlas, &fitted) > max {
        fitted.pop();
    }
    lines.push((fitted, WHITE, *y));
    *y += 11.0;
}

fn world_row(world: &WorldRow) -> String {
    let mut text = format!(
        "{} · {} · {}/{}",
        world.name, world.host_name, world.players, world.max
    );
    if !world.compatible {
        text.push_str(" · another version");
    } else if world.policy != Policy::Open {
        text.push_str(&format!(" · {}", world.policy.label()));
    }
    if world.direct {
        text.push_str(" · LAN");
    }
    text
}

fn person_row(person: &PersonRow) -> String {
    let mut text = format!("{} · {:?}", person.name, person.status);
    if !person.compatible {
        text.push_str(" · another version");
    }
    if person.direct {
        text.push_str(" · LAN");
    }
    text
}

fn invite_row(invite: &InviteRow) -> String {
    format!("{} invited you to {}", invite.host_name, invite.world_name)
}

fn request_row(request: &RequestRow) -> String {
    format!(
        "{} wants to join {}",
        request.guest_name, request.world_name
    )
}

fn button_text(label: &Text) -> Text {
    label.clone()
}

/// Draw the screen.
pub fn draw(
    batch: &mut HudBatch,
    atlas: &HudAtlas,
    cursor: (f32, f32),
    view: &NearbyView,
    in_world: bool,
) {
    let (w, h) = (batch.width, batch.height);
    crate::widgets::begin_screen(|| crate::widgets::ScreenInfo::new("nearby", TITLE));
    ui::menu_background(batch, atlas, 0.0, 0.0, w, h, in_world);
    let title = Text::literal(TITLE);
    let tw = atlas.font.width_text(&title);
    batch.text_styled(atlas, &title, ((w - tw) / 2.0).floor(), 12.0, WHITE, true);
    let (lines, hots) = layout(atlas, (w, h), view);
    for (text, color, y) in lines {
        if y + 9.0 > h - 28.0 {
            break;
        }
        batch.text(
            atlas,
            &text,
            ((w - HudBatch::text_width(atlas, &text)) / 2.0).floor(),
            y,
            color,
            true,
        );
    }
    for hot in &hots {
        let hover = hot.active && ui::hovered(cursor, hot.x, hot.y, hot.w, hot.h);
        crate::widgets::record(&hot.label.plain(), [hot.x, hot.y, hot.w, hot.h], hot.active);
        ui::button(
            batch,
            atlas,
            hot.x,
            hot.y,
            hot.w,
            hot.h,
            &button_text(&hot.label),
            hot.active,
            hover,
        );
    }
}

/// The button under a click, if it is active.
pub fn click(
    atlas: &HudAtlas,
    size: (f32, f32),
    cursor: (f32, f32),
    view: &NearbyView,
) -> Option<Action> {
    layout(atlas, size, view)
        .1
        .into_iter()
        .rev()
        .find(|h| h.active && ui::hovered(cursor, h.x, h.y, h.w, h.h))
        .map(|h| h.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas() -> HudAtlas {
        HudAtlas::build(std::path::Path::new("/nonexistent/horizon-test-assets")).unwrap()
    }

    fn view() -> NearbyView {
        NearbyView {
            in_world: true,
            snapshot: Snapshot {
                enabled: true,
                room: Some("auto-12ab34cd56ef".into()),
                sharing: Some(horizon_nearby::service::SharedWorld {
                    world_id: "0123456789abcdef".into(),
                    name: "Test world".into(),
                    players: 1,
                    max: 8,
                }),
                worlds: vec![WorldRow {
                    world_id: "abcdef0123456789".into(),
                    name: "Steve's world".into(),
                    host_id: "1111111111111111".into(),
                    host_name: "Steve".into(),
                    players: 1,
                    max: 8,
                    policy: Policy::Ask,
                    compatible: true,
                    direct: true,
                }],
                people: vec![PersonRow {
                    id: "2222222222222222".into(),
                    name: "Alex".into(),
                    status: horizon_nearby::proto::PlayerStatus::Menu,
                    compatible: true,
                    direct: true,
                }],
                requests: vec![RequestRow {
                    id: "req".into(),
                    guest_name: "Herobrine".into(),
                    world_name: "Test world".into(),
                }],
                invites: vec![InviteRow {
                    id: "inv".into(),
                    host_name: "Steve".into(),
                    world_name: "Steve's world".into(),
                }],
                ..Snapshot::default()
            },
            copied: false,
        }
    }

    #[test]
    fn a_join_request_can_be_accepted_or_denied() {
        let atlas = atlas();
        let size = (427.0, 240.0);
        let view = view();
        let (_, hots) = layout(&atlas, size, &view);
        for action in [Action::Accept("req".into()), Action::Deny("req".into())] {
            let hot = hots.iter().find(|h| h.action == action).unwrap();
            assert_eq!(
                click(&atlas, size, (hot.x + 1.0, hot.y + 1.0), &view),
                Some(action)
            );
        }
    }

    #[test]
    fn worlds_people_and_invites_are_actionable() {
        let atlas = atlas();
        let size = (427.0, 240.0);
        let view = view();
        let (_, hots) = layout(&atlas, size, &view);
        for action in [
            Action::Join("abcdef0123456789".into()),
            Action::Invite("2222222222222222".into()),
            Action::AcceptInvite("inv".into()),
        ] {
            let hot = hots.iter().find(|h| h.action == action).unwrap();
            assert!(hot.active);
            assert_eq!(
                click(&atlas, size, (hot.x + 1.0, hot.y + 1.0), &view),
                Some(action)
            );
        }
        let share = hots.iter().find(|h| h.action == Action::StopShare).unwrap();
        assert_eq!(
            click(&atlas, size, (share.x + 1.0, share.y + 1.0), &view),
            Some(Action::StopShare)
        );
    }

    #[test]
    fn a_server_version_mismatch_disables_join() {
        let atlas = atlas();
        let size = (427.0, 240.0);
        let mut view = view();
        view.snapshot.worlds[0].compatible = false;
        let (_, hots) = layout(&atlas, size, &view);
        let join = hots
            .iter()
            .find(|h| h.action == Action::Join("abcdef0123456789".into()))
            .unwrap();
        assert!(!join.active);
        assert_eq!(
            click(&atlas, size, (join.x + 1.0, join.y + 1.0), &view),
            None
        );
    }
}
