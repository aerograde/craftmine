//! Protocol 47 packet bodies: the login, and the play packets a guest needs
//! to enter and see a world. 1.8 strings are VarInt-length UTF-16BE, not the
//! UTF-8 the modern protocol uses, and positions pack into a long.
use super::{LegacyState, ids};
use crate::{
    Error, Result,
    buf::{Reader, Writer},
};

/// Java's maximum string length in chars.
const MAX_STRING: usize = 32767;

/// A 1.8 string: VarInt UTF-16 code units, big-endian.
pub fn write_string(w: &mut Writer, text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(MAX_STRING).collect();
    w.varint(units.len() as i32);
    for unit in units {
        w.data.extend_from_slice(&unit.to_be_bytes());
    }
}

pub fn read_string(r: &mut Reader) -> Result<String> {
    let len = r.varint()?;
    let len = usize::try_from(len).map_err(|_| Error::new("negative string length"))?;
    if len > MAX_STRING {
        return Err(Error::new("string longer than protocol 47 allows"));
    }
    let bytes = r.bytes(len * 2)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| Error::new("invalid UTF-16 string"))
}

/// `BlockPos.asLong`: 26 bits of x, 12 of y, 26 of z.
pub fn write_position(w: &mut Writer, x: i32, y: i32, z: i32) {
    let packed = (i64::from(x) & 0x3FF_FFFF) << 38
        | (i64::from(y) & 0xFFF) << 26
        | (i64::from(z) & 0x3FF_FFFF);
    w.i64(packed);
}

pub fn read_position(r: &mut Reader) -> Result<(i32, i32, i32)> {
    let packed = r.i64()?;
    let x = ((packed >> 38) as i32) << 6 >> 6;
    let y = ((packed >> 26) as i32) << 20 >> 20;
    let z = (packed as i32) << 6 >> 6;
    Ok((x, y, z))
}

/// A client's `Login Start`: its name.
pub fn read_login_start(id: i32, payload: &[u8]) -> Result<String> {
    if id != ids::login::serverbound::LOGIN_START {
        return Err(Error::new("expected a login start"));
    }
    read_string(&mut Reader::new(payload))
}

/// `Login Success`: the UUID as text, then the name.
pub fn login_success(uuid: &str, name: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::login::clientbound::LOGIN_SUCCESS);
    write_string(&mut w, uuid);
    write_string(&mut w, name);
    w.data
}

pub fn set_compression(threshold: i32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::login::clientbound::SET_COMPRESSION);
    w.varint(threshold);
    w.data
}

pub fn disconnect(reason_json: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::login::clientbound::DISCONNECT);
    write_string(&mut w, reason_json);
    w.data
}

/// `Join Game`: the entity, the game rules the client draws with, and the
/// level type (1.8 has one world height and no registry data).
pub fn join_game(
    entity_id: i32,
    gamemode: u8,
    dimension: i8,
    difficulty: u8,
    max_players: u8,
    level_type: &str,
    reduced_debug: bool,
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::JOIN_GAME);
    w.i32(entity_id);
    w.u8(gamemode);
    w.i8(dimension);
    w.u8(difficulty);
    w.u8(max_players);
    write_string(&mut w, level_type);
    w.bool(reduced_debug);
    w.data
}

/// `Spawn Position`: the world's spawn block.
pub fn spawn_position(x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::SPAWN_POSITION);
    write_position(&mut w, x, y, z);
    w.data
}

/// `Player Position And Look`: where the guest stands, and the flags saying
/// which of the values to take as absolute.
pub fn position_and_look(x: f64, y: f64, z: f64, yaw: f32, pitch: f32, flags: u8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::PLAYER_POSITION_AND_LOOK);
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.f32(yaw);
    w.f32(pitch);
    w.u8(flags);
    w.data
}

/// `Multi Block Change`: the blocks of one section that changed, 1.8's
/// compact form (chunk column, count, then position and state shorts).
pub fn multi_block_change(
    chunk_x: i32,
    chunk_z: i32,
    changes: &[([u8; 3], LegacyState)],
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::MULTI_BLOCK_CHANGE);
    w.i32(chunk_x);
    w.i32(chunk_z);
    w.varint(changes.len() as i32);
    for ([x, y, z], state) in changes {
        let position = u16::from(*x & 15) << 12 | u16::from(*z & 15) << 8 | u16::from(*y & 15);
        w.u16(position);
        w.u16(state.packed());
    }
    w.data
}

/// `Block Change`: one block of the world.
pub fn block_change(x: i32, y: i32, z: i32, state: LegacyState) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::BLOCK_CHANGE);
    write_position(&mut w, x, y, z);
    w.varint(i32::from(state.packed()));
    w.data
}

/// A world coordinate as 1.8's fixed-point (1/32 block).
fn fixed(value: f64) -> i32 {
    (value * 32.0)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// A velocity as 1.8's fixed-point (1/8000 block per tick).
fn velocity(value: f64) -> i16 {
    (value * 8000.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// A 1.8 rotation byte from degrees (256 steps per turn).
fn angle(degrees: f32) -> i8 {
    (degrees * 256.0 / 360.0).round() as i8
}

/// `Destroy Entities`: the entities that left the world.
pub fn destroy_entities(ids: &[i32]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::DESTROY_ENTITIES);
    w.varint(ids.len() as i32);
    for id in ids {
        w.varint(*id);
    }
    w.data
}

/// `Entity Velocity`: an entity's new movement.
pub fn entity_velocity(id: i32, movement: [f64; 3]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_VELOCITY);
    w.varint(id);
    for value in movement {
        w.i16(velocity(value));
    }
    w.data
}

/// `Rel Entity Move`: a small position change (1/32 block units).
pub fn rel_entity_move(id: i32, delta: [i8; 3], on_ground: bool) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_REL_MOVE);
    w.varint(id);
    for value in delta {
        w.i8(value);
    }
    w.bool(on_ground);
    w.data
}

/// `Entity Look`: a rotation change (256-step angles).
pub fn entity_look(id: i32, yaw: i8, pitch: i8, on_ground: bool) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_LOOK);
    w.varint(id);
    w.i8(yaw);
    w.i8(pitch);
    w.bool(on_ground);
    w.data
}

/// `Entity Look and Rel Move`: both at once.
pub fn entity_look_and_rel_move(
    id: i32,
    delta: [i8; 3],
    yaw: i8,
    pitch: i8,
    on_ground: bool,
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_LOOK_AND_REL_MOVE);
    w.varint(id);
    for value in delta {
        w.i8(value);
    }
    w.i8(yaw);
    w.i8(pitch);
    w.bool(on_ground);
    w.data
}

/// `Entity Head Look`: a head yaw (256-step angles).
pub fn entity_head_look(id: i32, head_yaw: i8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_HEAD_LOOK);
    w.varint(id);
    w.i8(head_yaw);
    w.data
}

/// `Entity Equipment`: the worn and held stacks of an entity.
pub fn entity_equipment(entity: i32, entries: &[(i16, Vec<u8>)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_EQUIPMENT);
    w.varint(entity);
    for (slot, item) in entries {
        w.i16(*slot);
        w.data.extend_from_slice(item);
    }
    w.i16(-1);
    w.data
}

/// `Set Slot`: one stack in a window.
pub fn set_slot(window: i8, slot: i16, item: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::SET_SLOT);
    w.i8(window);
    w.i16(slot);
    w.data.extend_from_slice(item);
    w.data
}

/// `Window Items`: every stack in a window and the carried stack.
pub fn window_items(window: i8, items: &[Vec<u8>], carried: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::WINDOW_ITEMS);
    w.i8(window);
    w.i16(items.len() as i16);
    for item in items {
        w.data.extend_from_slice(item);
    }
    w.data.extend_from_slice(carried);
    w.data
}

/// `Window Property`: a container's progress bar value.
pub fn window_property(window: i8, property: i16, value: i16) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::WINDOW_PROPERTY);
    w.i8(window);
    w.i16(property);
    w.i16(value);
    w.data
}

/// `Close Window`: the guest's open container.
pub fn close_window(window: i8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::CLOSE_WINDOW);
    w.i8(window);
    w.data
}

/// `Entity Teleport`: an absolute position and look.
pub fn entity_teleport(
    id: i32,
    position: [f64; 3],
    yaw: f32,
    pitch: f32,
    on_ground: bool,
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ENTITY_TELEPORT);
    w.varint(id);
    for value in position {
        w.i32(fixed(value));
    }
    w.i8(angle(yaw));
    w.i8(angle(pitch));
    w.bool(on_ground);
    w.data
}

/// `Spawn Mob`: a living entity, with no metadata beyond the terminator.
pub fn spawn_mob(
    entity: i32,
    kind: u8,
    position: [f64; 3],
    angles: [i8; 3],
    movement: [f64; 3],
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::SPAWN_MOB);
    w.varint(entity);
    w.u8(kind);
    for value in position {
        w.i32(fixed(value));
    }
    for angle in angles {
        w.i8(angle);
    }
    for value in movement {
        w.i16(velocity(value));
    }
    w.u8(0x7F); // no metadata
    w.data
}

/// `Spawn Object`: an item, projectile, vehicle and so on.
pub fn spawn_object(
    entity: i32,
    kind: u8,
    position: [f64; 3],
    angles: [i8; 2],
    data: i32,
    movement: [f64; 3],
) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::SPAWN_OBJECT);
    w.varint(entity);
    w.u8(kind);
    for value in position {
        w.i32(fixed(value));
    }
    for angle in angles {
        w.i8(angle);
    }
    w.i32(data);
    for value in movement {
        w.i16(velocity(value));
    }
    w.data
}

/// `Keep Alive`: the id the guest echoes back.
pub fn keep_alive(id: i32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::KEEP_ALIVE);
    w.varint(id);
    w.data
}

/// `Time Update`: the world's age and its time of day.
pub fn time_update(world_age: i64, time_of_day: i64) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::TIME_UPDATE);
    w.i64(world_age);
    w.i64(time_of_day);
    w.data
}

/// `Chat Message`: a JSON component and where it shows (0 chat, 1 system,
/// 2 above the hotbar).
pub fn chat(json: &str, position: u8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::CHAT);
    write_string(&mut w, json);
    w.u8(position);
    w.data
}

/// `Update Health`.
pub fn update_health(health: f32, food: i32, saturation: f32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::UPDATE_HEALTH);
    w.f32(health);
    w.varint(food);
    w.f32(saturation);
    w.data
}

/// `Respawn` (1.8's dimension is an int, and there is no world seed).
pub fn respawn(dimension: i32, difficulty: u8, gamemode: u8, level_type: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::RESPAWN);
    w.i32(dimension);
    w.u8(difficulty);
    w.u8(gamemode);
    write_string(&mut w, level_type);
    w.data
}

/// `Player Abilities`: the flags, the flying speed and the field-of-view
/// modifier.
pub fn player_abilities(flags: i8, flying_speed: f32, field_of_view: f32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::PLAYER_ABILITIES);
    w.i8(flags);
    w.f32(flying_speed);
    w.f32(field_of_view);
    w.data
}

/// `Held Item Change`: the selected hotbar slot.
pub fn held_item_change(slot: i8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::HELD_ITEM_CHANGE);
    w.i8(slot);
    w.data
}

/// `Animation`: a swing of an entity's arm.
pub fn animation(entity: i32, animation: u8) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::ANIMATION);
    w.varint(entity);
    w.u8(animation);
    w.data
}

/// `Change Game State`: a reason and its value.
pub fn game_state(reason: u8, value: f32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(ids::play::clientbound::CHANGE_GAME_STATE);
    w.u8(reason);
    w.f32(value);
    w.data
}

/// A guest's movement packet: `Player` (on ground only), `Player Position`,
/// `Player Look` or `Player Position And Look`, as the host feeds them to the
/// world's server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Movement {
    pub position: Option<(f64, f64, f64)>,
    pub look: Option<(f32, f32)>,
    pub on_ground: bool,
}

/// Reads a serverbound movement packet; `None` for another packet id.
pub fn read_movement(id: i32, body: &[u8]) -> Result<Option<Movement>> {
    let mut r = Reader::new(body);
    Ok(match id {
        ids::play::serverbound::PLAYER => Some(Movement {
            position: None,
            look: None,
            on_ground: r.bool()?,
        }),
        ids::play::serverbound::PLAYER_POSITION => Some(Movement {
            position: Some((r.f64()?, r.f64()?, r.f64()?)),
            look: None,
            on_ground: r.bool()?,
        }),
        ids::play::serverbound::PLAYER_LOOK => Some(Movement {
            position: None,
            look: Some((r.f32()?, r.f32()?)),
            on_ground: r.bool()?,
        }),
        ids::play::serverbound::PLAYER_POSITION_AND_LOOK => Some(Movement {
            position: Some((r.f64()?, r.f64()?, r.f64()?)),
            look: Some((r.f32()?, r.f32()?)),
            on_ground: r.bool()?,
        }),
        _ => None,
    })
}

/// `Keep Alive` from the guest: the id it echoed.
pub fn read_keep_alive(id: i32, payload: &[u8]) -> Result<i32> {
    if id != ids::play::serverbound::KEEP_ALIVE {
        return Err(Error::new("expected a keep alive"));
    }
    Reader::new(payload).varint()
}

/// `Chat Message` from the guest.
pub fn read_chat(id: i32, payload: &[u8]) -> Result<String> {
    if id != ids::play::serverbound::CHAT {
        return Err(Error::new("expected a chat message"));
    }
    read_string(&mut Reader::new(payload))
}

/// `Held Item Change` from the guest: the hotbar slot, a short.
pub fn read_held_item(id: i32, payload: &[u8]) -> Result<u16> {
    if id != ids::play::serverbound::HELD_ITEM_CHANGE {
        return Err(Error::new("expected a held item change"));
    }
    Reader::new(payload).u16()
}

/// `Client Settings` from the guest: the 1.8 fields a server reads.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientSettings {
    pub locale: String,
    pub view_distance: i8,
    pub chat_mode: u8,
    pub chat_colors: bool,
    pub skin_parts: u8,
    pub main_hand: u8,
}

pub fn read_client_settings(id: i32, payload: &[u8]) -> Result<ClientSettings> {
    if id != ids::play::serverbound::CLIENT_SETTINGS {
        return Err(Error::new("expected client settings"));
    }
    let mut r = Reader::new(payload);
    Ok(ClientSettings {
        locale: read_string(&mut r)?,
        view_distance: r.i8()?,
        chat_mode: r.u8()?,
        chat_colors: r.bool()?,
        skin_parts: r.u8()?,
        main_hand: r.u8()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_varint_length_utf16() {
        let mut w = Writer::new();
        write_string(&mut w, "Hi\u{1F600}");
        // Four UTF-16 code units: H, i and the emoji's surrogate pair.
        assert_eq!(&w.data[..3], &[0x04, 0x00, b'H']);
        assert_eq!(&w.data[3..], &[0x00, b'i', 0xD8, 0x3D, 0xDE, 0x00]);
        let mut r = Reader::new(&w.data);
        assert_eq!(read_string(&mut r).unwrap(), "Hi\u{1F600}");
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn login_packets_read_and_write() {
        let mut name = Writer::new();
        write_string(&mut name, "Steve");
        assert_eq!(
            read_login_start(ids::login::serverbound::LOGIN_START, &name.data).unwrap(),
            "Steve"
        );

        let success = login_success("069a79f4-44e9-4726-a5be-fca90e38aaf5", "Steve");
        let mut r = Reader::new(&success);
        assert_eq!(r.varint().unwrap(), ids::login::clientbound::LOGIN_SUCCESS);
        assert_eq!(
            read_string(&mut r).unwrap(),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        assert_eq!(read_string(&mut r).unwrap(), "Steve");

        assert_eq!(set_compression(64), [0x03, 0x40]);
        let bye = disconnect("{\"text\":\"bye\"}");
        let mut r = Reader::new(&bye);
        assert_eq!(r.varint().unwrap(), ids::login::clientbound::DISCONNECT);
        assert_eq!(read_string(&mut r).unwrap(), "{\"text\":\"bye\"}");
    }

    #[test]
    fn positions_pack_like_1_8() {
        let mut w = Writer::new();
        write_position(&mut w, 1, 2, 3);
        let packed = (1i64 << 38) | (2i64 << 26) | 3;
        assert_eq!(w.data, packed.to_be_bytes());
        for (x, y, z) in [
            (0, 0, 0),
            (-1, -1, -1),
            (-33554432, -2048, 33554431),
            (100, 64, -200),
        ] {
            let mut w = Writer::new();
            write_position(&mut w, x, y, z);
            let mut r = Reader::new(&w.data);
            assert_eq!(read_position(&mut r).unwrap(), (x, y, z));
        }
    }

    #[test]
    fn world_entry_packets_are_shaped_like_1_8() {
        let join = join_game(7, 1, 0, 2, 20, "default", false);
        let mut r = Reader::new(&join);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::JOIN_GAME);
        assert_eq!(r.i32().unwrap(), 7);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 2);
        assert_eq!(r.u8().unwrap(), 20);
        assert_eq!(read_string(&mut r).unwrap(), "default");
        assert!(!r.bool().unwrap());
        assert_eq!(r.remaining(), 0);

        let look = position_and_look(1.5, 64.0, -2.5, 90.0, -10.0, 0);
        assert_eq!(look.len(), 1 + 24 + 4 + 4 + 1);
        let mut r = Reader::new(&look);
        assert_eq!(
            r.varint().unwrap(),
            ids::play::clientbound::PLAYER_POSITION_AND_LOOK
        );
        assert_eq!((r.f64().unwrap(), r.f64().unwrap()), (1.5, 64.0));
        assert_eq!(r.f64().unwrap(), -2.5);
        assert_eq!((r.f32().unwrap(), r.f32().unwrap()), (90.0, -10.0));
        assert_eq!(r.u8().unwrap(), 0);

        let spawn = spawn_position(0, 64, -16);
        let mut r = Reader::new(&spawn);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::SPAWN_POSITION);
        assert_eq!(read_position(&mut r).unwrap(), (0, 64, -16));

        let change = block_change(3, 70, -4, LegacyState { id: 5, data: 2 });
        let mut r = Reader::new(&change);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::BLOCK_CHANGE);
        assert_eq!(read_position(&mut r).unwrap(), (3, 70, -4));
        assert_eq!(r.varint().unwrap(), 0x52);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn status_and_chat_packets_round_trip() {
        let alive = keep_alive(42);
        let mut r = Reader::new(&alive);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::KEEP_ALIVE);
        assert_eq!(r.varint().unwrap(), 42);

        let time = time_update(1000, 6000);
        let mut r = Reader::new(&time);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::TIME_UPDATE);
        assert_eq!((r.i64().unwrap(), r.i64().unwrap()), (1000, 6000));

        let message = chat("{\"text\":\"hi\"}", 1);
        let mut r = Reader::new(&message);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::CHAT);
        assert_eq!(read_string(&mut r).unwrap(), "{\"text\":\"hi\"}");
        assert_eq!(r.u8().unwrap(), 1);

        let health = update_health(20.0, 20, 5.0);
        let mut r = Reader::new(&health);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::UPDATE_HEALTH);
        assert_eq!((r.f32().unwrap(), r.varint().unwrap()), (20.0, 20));
        assert_eq!(r.f32().unwrap(), 5.0);

        let re = respawn(0, 2, 1, "default");
        let mut r = Reader::new(&re);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::RESPAWN);
        assert_eq!(
            (r.i32().unwrap(), r.u8().unwrap(), r.u8().unwrap()),
            (0, 2, 1)
        );
        assert_eq!(read_string(&mut r).unwrap(), "default");

        let abilities = player_abilities(0x0F, 0.05, 0.1);
        let mut r = Reader::new(&abilities);
        assert_eq!(
            r.varint().unwrap(),
            ids::play::clientbound::PLAYER_ABILITIES
        );
        assert_eq!(r.i8().unwrap(), 0x0F);
        assert_eq!((r.f32().unwrap(), r.f32().unwrap()), (0.05, 0.1));

        let held = held_item_change(3);
        let mut r = Reader::new(&held);
        assert_eq!(
            r.varint().unwrap(),
            ids::play::clientbound::HELD_ITEM_CHANGE
        );
        assert_eq!(r.i8().unwrap(), 3);

        let swing = animation(5, 0);
        let mut r = Reader::new(&swing);
        assert_eq!(r.varint().unwrap(), ids::play::clientbound::ANIMATION);
        assert_eq!((r.varint().unwrap(), r.u8().unwrap()), (5, 0));

        let state = game_state(3, 1.0);
        let mut r = Reader::new(&state);
        assert_eq!(
            r.varint().unwrap(),
            ids::play::clientbound::CHANGE_GAME_STATE
        );
        assert_eq!((r.u8().unwrap(), r.f32().unwrap()), (3, 1.0));
    }

    #[test]
    fn serverbound_packets_read() {
        let mut look = Writer::new();
        look.f64(1.0);
        look.f64(64.0);
        look.f64(-2.5);
        look.f32(90.0);
        look.f32(-10.0);
        look.bool(true);
        assert_eq!(
            read_movement(ids::play::serverbound::PLAYER_POSITION_AND_LOOK, &look.data).unwrap(),
            Some(Movement {
                position: Some((1.0, 64.0, -2.5)),
                look: Some((90.0, -10.0)),
                on_ground: true,
            })
        );
        assert_eq!(
            read_movement(ids::play::serverbound::PLAYER, &[1]).unwrap(),
            Some(Movement {
                position: None,
                look: None,
                on_ground: true,
            })
        );
        assert_eq!(read_movement(999, &[]).unwrap(), None);

        let mut alive = Writer::new();
        alive.varint(42);
        assert_eq!(
            read_keep_alive(ids::play::serverbound::KEEP_ALIVE, &alive.data).unwrap(),
            42
        );

        let mut message = Writer::new();
        write_string(&mut message, "hello");
        assert_eq!(
            read_chat(ids::play::serverbound::CHAT, &message.data).unwrap(),
            "hello"
        );

        assert_eq!(
            read_held_item(ids::play::serverbound::HELD_ITEM_CHANGE, &[0, 3]).unwrap(),
            3
        );

        let mut settings = Writer::new();
        write_string(&mut settings, "en_US");
        settings.i8(12);
        settings.u8(0);
        settings.bool(true);
        settings.u8(0x7F);
        settings.u8(1);
        assert_eq!(
            read_client_settings(ids::play::serverbound::CLIENT_SETTINGS, &settings.data).unwrap(),
            ClientSettings {
                locale: "en_US".into(),
                view_distance: 12,
                chat_mode: 0,
                chat_colors: true,
                skin_parts: 0x7F,
                main_hand: 1,
            }
        );
    }
}
