//! The 26.2 game protocol (776) beside this world's 26.3 (777).
//!
//! The two releases register the same packets in the same order except for
//! three insertions: 26.3 added `add_transient_block` to play (clientbound
//! 37) and `post_effects` to configuration (clientbound 10), and it replaced
//! 26.2's `swing(hand)` with the empty `punch` (serverbound 46). The tables
//! below were derived from the 26.2 source's `GameProtocols` and
//! `ConfigurationProtocols` and this crate's generated 26.3 ids, whose game
//! lists align exactly once those three are accounted for.
//!
//! The ids are only half of a translation: a caller forwarding a packet must
//! also check that its body did not change between the releases. `punch` is
//! the one known body change here (26.2's swing carries a hand varint and
//! 26.3's punch is empty).
use crate::{
    buf::{Codec, Reader, Writer},
    chunk::{BLOCKS, ChunkData, Paletted, Strategy},
    packets::play::Clientbound,
    translate::mappings_26_2 as map,
};
use horizon_world::nbt::{JavaString, Tag};

pub const PROTOCOL_26_2: i32 = 776;
pub const PROTOCOL_26_3: i32 = 777;

/// What to do with one 26.2 serverbound play packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Serverbound {
    /// Forward this 26.3 id, body unchanged.
    Forward(i32),
    /// 26.2's `swing(hand)`; 26.3's empty `punch`. Drop the hand varint.
    Punch,
}

/// 26.3 clientbound play → 26.2, or `None` for packets 26.2 has no id for.
pub fn clientbound_play(id: i32) -> Option<i32> {
    match id {
        0..=36 => Some(id),
        // Three 26.3 insertions: add_transient_block at 37, post_effects at
        // 83, and swing_animation at 123. 26.2 has no id for any of them.
        37 | 83 | 123 => None,
        38..=82 => Some(id - 1),
        84..=122 => Some(id - 2),
        124..=143 => Some(id - 3),
        _ => None,
    }
}

/// 26.2 serverbound play → 26.3, or `None` for an id neither release has.
pub fn serverbound_play(id: i32) -> Option<Serverbound> {
    match id {
        0..=45 => Some(Serverbound::Forward(id)),
        // 26.3 inserted `punch` at 46, moving these one place.
        46..=62 => Some(Serverbound::Forward(id + 1)),
        // 26.2's swing became 26.3's punch (and lost its hand field).
        63 => Some(Serverbound::Punch),
        64..=68 => Some(Serverbound::Forward(id)),
        _ => None,
    }
}

/// 26.3 clientbound configuration → 26.2, or `None` for 26.2-unavailable ids.
pub fn clientbound_configuration(id: i32) -> Option<i32> {
    match id {
        0..=9 => Some(id),
        // 26.3's `post_effects`; 26.2's configuration has no such packet.
        10 => None,
        11..=20 => Some(id - 1),
        _ => None,
    }
}

/// 26.2 serverbound configuration → 26.3: the lists and ids are identical.
pub fn serverbound_configuration(id: i32) -> Option<i32> {
    (0..=9).contains(&id).then_some(id)
}

/// 26.2 clientbound play → 26.3, the inverse of [`clientbound_play`].
pub fn clientbound_from_26_2(id: i32) -> Option<i32> {
    match id {
        0..=36 => Some(id),
        37..=81 => Some(id + 1),
        82..=120 => Some(id + 2),
        121..=140 => Some(id + 3),
        _ => None,
    }
}

/// 26.3 serverbound play → 26.2, the inverse of [`serverbound_play`].
pub fn serverbound_play_from_26_3(id: i32) -> Option<i32> {
    match id {
        0..=45 => Some(id),
        46 => Some(63),
        47..=63 => Some(id - 1),
        64..=68 => Some(id),
        _ => None,
    }
}

/// One output packet: its mapped id and capacity for the original body.
fn output(capacity: usize, id: i32) -> Writer {
    let mut w = Writer::new();
    w.data.reserve(capacity + 5);
    w.varint(id);
    w
}

/// One 26.3 clientbound play packet body → the 26.2 guest's, or `None` when
/// the packet has no 26.2 form. One allocation.
pub fn rewrite_clientbound_play(body: &[u8]) -> Option<Vec<u8>> {
    let (id, rest) = split(body)?;
    // `swing_animation` has no 26.2 id of its own: it becomes `animate`.
    if id == crate::ids::play::clientbound::SWING_ANIMATION {
        return remap_swing_animation(rest);
    }
    let mut w = output(body.len(), clientbound_play(id)?);
    match id {
        crate::ids::play::clientbound::RECIPE_BOOK_ADD => {
            w.bytes(&crate::translate::recipe_display::book_add(rest)?)
        }
        crate::ids::play::clientbound::PLACE_GHOST_RECIPE => {
            w.bytes(&crate::translate::recipe_display::place_ghost_recipe(rest)?)
        }
        crate::ids::play::clientbound::UPDATE_RECIPES => {
            w.bytes(&crate::translate::recipe_display::update_recipes(rest)?)
        }
        crate::ids::play::clientbound::LOGIN => login_to_26_2(rest, &mut w)?,
        crate::ids::play::clientbound::SET_ENTITY_DATA => set_entity_data_to_26_2(rest, &mut w)?,
        crate::ids::play::clientbound::ADD_ENTITY => w.bytes(&remap_add_entity(rest, |kind| {
            map::to_26_2(&map::ENTITIES, kind)
        })?),
        crate::ids::play::clientbound::MOVE_ENTITY_POS => {
            w.bytes(&remap_move_26_3(rest, Move::Pos)?)
        }
        crate::ids::play::clientbound::MOVE_ENTITY_POS_ROT => {
            w.bytes(&remap_move_26_3(rest, Move::PosRot)?)
        }
        crate::ids::play::clientbound::MOVE_ENTITY_ROT => {
            w.bytes(&remap_move_26_3(rest, Move::Rot)?)
        }
        crate::ids::play::clientbound::ANIMATE => w.bytes(&remap_animate(rest, true)?),
        crate::ids::play::clientbound::SWING_ANIMATION => return remap_swing_animation(rest),
        crate::ids::play::clientbound::BLOCK_UPDATE => {
            w.bytes(&remap_block_update(rest, map::block_state_to_26_2)?)
        }
        crate::ids::play::clientbound::SECTION_BLOCKS_UPDATE => {
            w.bytes(&remap_section_blocks(rest, map::block_state_to_26_2)?)
        }
        crate::ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
            w.bytes(&remap_chunk(rest, map::block_state_to_26_2)?)
        }
        crate::ids::play::clientbound::BLOCK_EVENT => w.bytes(&remap_block_event(rest)?),
        crate::ids::play::clientbound::COMMANDS => w.bytes(&remap_commands(rest)?),
        crate::ids::play::clientbound::ENTITY_POSITION_SYNC => w.bytes(&remap_position_sync(rest)?),
        crate::ids::play::clientbound::PLAYER_CHAT
        | crate::ids::play::clientbound::SYSTEM_CHAT
        | crate::ids::play::clientbound::DISGUISED_CHAT => {
            w.bytes(&remap_chat_components(id, rest)?)
        }
        crate::ids::play::clientbound::LEVEL_PARTICLES => w.bytes(&remap_particles(rest, true)?),
        crate::ids::play::clientbound::SOUND | crate::ids::play::clientbound::SOUND_ENTITY => {
            w.bytes(&remap_sound(id, rest, true)?)
        }
        crate::ids::play::clientbound::CONTAINER_SET_CONTENT
        | crate::ids::play::clientbound::CONTAINER_SET_SLOT
        | crate::ids::play::clientbound::SET_CURSOR_ITEM
        | crate::ids::play::clientbound::SET_PLAYER_INVENTORY
        | crate::ids::play::clientbound::SET_EQUIPMENT => w.bytes(&remap_items(id, rest, true)?),
        _ => w.bytes(rest),
    }
    Some(w.data)
}

/// One 26.2 serverbound play packet body → the 26.3 world's, or `None` for an
/// unknown id. One allocation.
pub fn rewrite_serverbound_play(body: &[u8]) -> Option<Vec<u8>> {
    let (id, rest) = split(body)?;
    let forward = match serverbound_play(id)? {
        Serverbound::Forward(id) => Some(id),
        // 26.2's swing carries the hand; 26.3's punch is empty.
        Serverbound::Punch => None,
    };
    let id = forward.unwrap_or(crate::ids::play::serverbound::PUNCH);
    let mut w = output(body.len(), id);
    if forward == Some(crate::ids::play::serverbound::PLAYER_ACTION) {
        w.bytes(&remap_player_action(rest, true)?);
    } else if forward.is_some() {
        w.bytes(rest);
    }
    Some(w.data)
}

fn remap_player_action(rest: &[u8], to_26_3: bool) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let action = r.varint().ok()?;
    let action = if to_26_3 {
        if action >= 1 { action + 1 } else { action }
    } else if action >= 2 {
        action - 1
    } else {
        action
    };
    let tail = r.rest();
    let mut w = Writer::new();
    w.varint(action);
    w.data.extend_from_slice(tail);
    Some(w.data)
}

/// One 26.2 clientbound play body from a host → this world's client: the
/// inverse of [`clientbound_play`]. One allocation.
pub fn rewrite_clientbound_from_26_2(body: &[u8]) -> Option<Vec<u8>> {
    let (original, rest) = split(body)?;
    let id = clientbound_from_26_2(original)?;
    // Same recipe-body incompatibility in the 26.2 host -> 26.3 guest path.
    if id == crate::ids::play::clientbound::RECIPE_BOOK_ADD
        || id == crate::ids::play::clientbound::PLACE_GHOST_RECIPE
        || id == crate::ids::play::clientbound::UPDATE_RECIPES
    {
        return None;
    }
    let mut w = output(body.len(), id);
    match id {
        crate::ids::play::clientbound::LOGIN => login_from_26_2(rest, &mut w)?,
        crate::ids::play::clientbound::SET_ENTITY_DATA => set_entity_data_from_26_2(rest, &mut w)?,
        crate::ids::play::clientbound::ADD_ENTITY => w.bytes(&remap_add_entity(rest, |kind| {
            map::to_26_3(&map::ENTITIES, kind)
        })?),
        crate::ids::play::clientbound::MOVE_ENTITY_POS => {
            w.bytes(&remap_move_26_2(rest, Move::Pos)?)
        }
        crate::ids::play::clientbound::MOVE_ENTITY_POS_ROT => {
            w.bytes(&remap_move_26_2(rest, Move::PosRot)?)
        }
        crate::ids::play::clientbound::MOVE_ENTITY_ROT => {
            w.bytes(&remap_move_26_2(rest, Move::Rot)?)
        }
        crate::ids::play::clientbound::ANIMATE => w.bytes(&remap_animate(rest, false)?),
        crate::ids::play::clientbound::ENTITY_POSITION_SYNC => {
            w.bytes(&remap_position_sync_from_26_2(rest)?)
        }
        crate::ids::play::clientbound::BLOCK_UPDATE => {
            w.bytes(&remap_block_update(rest, map::block_state_to_26_3)?)
        }
        crate::ids::play::clientbound::SECTION_BLOCKS_UPDATE => {
            w.bytes(&remap_section_blocks(rest, map::block_state_to_26_3)?)
        }
        crate::ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
            w.bytes(&remap_chunk(rest, map::block_state_to_26_3)?)
        }
        crate::ids::play::clientbound::BLOCK_EVENT => w.bytes(&remap_block_event(rest)?),
        crate::ids::play::clientbound::LEVEL_PARTICLES => w.bytes(&remap_particles(rest, false)?),
        crate::ids::play::clientbound::SOUND | crate::ids::play::clientbound::SOUND_ENTITY => {
            w.bytes(&remap_sound(id, rest, false)?)
        }
        crate::ids::play::clientbound::CONTAINER_SET_CONTENT
        | crate::ids::play::clientbound::CONTAINER_SET_SLOT
        | crate::ids::play::clientbound::SET_CURSOR_ITEM
        | crate::ids::play::clientbound::SET_PLAYER_INVENTORY
        | crate::ids::play::clientbound::SET_EQUIPMENT => w.bytes(&remap_items(id, rest, false)?),
        _ => w.bytes(rest),
    }
    Some(w.data)
}

#[derive(Clone, Copy)]
enum Move {
    Pos,
    PosRot,
    Rot,
}

fn remap_move_26_3(rest: &[u8], kind: Move) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let mut w = Writer::new();
    w.varint(entity);
    if matches!(kind, Move::Rot) {
        let on_ground = r.bool().ok()?;
        let yaw = r.i8().ok()?;
        let pitch = r.i8().ok()?;
        w.i8(yaw);
        w.i8(pitch);
        w.bool(on_ground);
        return Some(w.data);
    }
    let properties = r.varint().ok()?;
    let steps = properties >> 1;
    let on_ground = properties & 1 != 0;
    let mut delta = [0i32; 3];
    if steps == 0 {
        for value in &mut delta {
            *value = i32::from(r.i16().ok()?);
        }
    } else {
        for _ in 0..steps {
            r.varint().ok()?; // tick delay
            for value in &mut delta {
                *value += i32::from(r.i16().ok()?);
            }
        }
    }
    for value in delta {
        w.i16(value as i16);
    }
    if matches!(kind, Move::PosRot) {
        w.i8(r.i8().ok()?);
        w.i8(r.i8().ok()?);
    }
    w.bool(on_ground);
    Some(w.data)
}

fn remap_move_26_2(rest: &[u8], kind: Move) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let mut w = Writer::new();
    w.varint(entity);
    if matches!(kind, Move::Rot) {
        let yaw = r.i8().ok()?;
        let pitch = r.i8().ok()?;
        let on_ground = r.bool().ok()?;
        w.bool(on_ground);
        w.i8(yaw);
        w.i8(pitch);
        return Some(w.data);
    }
    let mut delta = [0i16; 3];
    for value in &mut delta {
        *value = r.i16().ok()?;
    }
    let properties = i32::from(r.bool().ok()?);
    w.varint(properties);
    for value in delta {
        w.i16(value);
    }
    if matches!(kind, Move::PosRot) {
        w.i8(r.i8().ok()?);
        w.i8(r.i8().ok()?);
    }
    Some(w.data)
}

fn remap_animate(rest: &[u8], to_26_2: bool) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let action = r.u8().ok()?;
    let action = if to_26_2 {
        match action {
            0 => 2,
            1 => 4,
            2 => 5,
            other => other,
        }
    } else {
        match action {
            2 => 0,
            4 => 1,
            5 => 2,
            other => other,
        }
    };
    let tail = r.rest();
    let mut w = Writer::new();
    w.varint(entity);
    w.u8(action);
    w.data.extend_from_slice(tail);
    Some(w.data)
}

fn remap_swing_animation(rest: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let hand = r.varint().ok()?;
    let kind = r.varint().ok()?;
    if kind == 0 {
        return None;
    }
    let mut w = Writer::new();
    w.varint(crate::ids::play::clientbound::ANIMATE);
    w.varint(entity);
    w.u8(if hand == 0 { 0 } else { 3 });
    Some(w.data)
}

fn remap_add_entity(rest: &[u8], entity: impl Fn(i32) -> Option<i32>) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let id = r.varint().ok()?;
    let uuid = r.bytes(16).ok()?;
    let kind = r.varint().ok()?;
    let mapped = entity(kind)?;
    let tail = r.rest();
    let mut w = Writer::new();
    w.varint(id);
    w.data.extend_from_slice(uuid);
    w.varint(mapped);
    w.data.extend_from_slice(tail);
    Some(w.data)
}

fn remap_block_update(rest: &[u8], state: impl Fn(i32) -> i32) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let position = r.i64().ok()?;
    let value = r.varint().ok()?;
    let tail = r.rest();
    let mut w = Writer::new();
    w.i64(position);
    w.varint(state(value));
    w.data.extend_from_slice(tail);
    Some(w.data)
}

fn remap_section_blocks(rest: &[u8], state: impl Fn(i32) -> i32) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let section = r.i64().ok()?;
    let count = r.varint().ok()?;
    if count < 0 {
        return None;
    }
    let mut w = Writer::new();
    w.i64(section);
    w.varint(count);
    for _ in 0..count {
        let change = r.varlong().ok()?;
        let local = change & 0xFFF;
        let mapped = i64::from(state((change >> 12) as i32)) << 12;
        w.varlong(mapped | local);
    }
    Some(w.data)
}

fn remap_chunk(rest: &[u8], state: impl Fn(i32) -> i32) -> Option<Vec<u8>> {
    let mut packet =
        Clientbound::decode(crate::ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT, rest).ok()?;
    let Clientbound::LevelChunkWithLight(chunk) = &mut packet else {
        return None;
    };
    let mut sections = chunk.chunk.all_sections().ok()?;
    for section in &mut sections {
        let values: Vec<i32> = (0..BLOCKS).map(|i| state(section.states.get(i))).collect();
        // 26.3's block states outgrow a 26.2 global palette; repack at the
        // width that holds both registries.
        section.states = Paletted::from_values(&values, Strategy::Blocks, 18);
    }
    chunk.chunk.buffer = ChunkData::from_sections(&sections);
    encode_body(&packet)
}

fn remap_position_sync(rest: &[u8]) -> Option<Vec<u8>> {
    use crate::packets::play::{Clientbound, PositionPath};
    let packet =
        Clientbound::decode(crate::ids::play::clientbound::ENTITY_POSITION_SYNC, rest).ok()?;
    let Clientbound::EntityPositionSync(sync) = packet else {
        return None;
    };
    let position = sync.path.end();
    let delta = match &sync.path {
        PositionPath::Linear(_) => [0.0; 3],
        PositionPath::Stepped(steps) if steps.len() > 1 => {
            let previous = steps[steps.len() - 2].0;
            let ticks = steps.last()?.1.max(1) as f64;
            [
                (position[0] - previous[0]) / ticks,
                (position[1] - previous[1]) / ticks,
                (position[2] - previous[2]) / ticks,
            ]
        }
        PositionPath::Stepped(_) => [0.0; 3],
    };
    let mut w = Writer::new();
    w.varint(sync.id);
    for value in position {
        w.f64(value);
    }
    for value in delta {
        w.f64(value);
    }
    w.f32(sync.yaw);
    w.f32(sync.pitch);
    w.bool(sync.on_ground);
    Some(w.data)
}

fn remap_position_sync_from_26_2(rest: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let mut position = [0.0; 3];
    for value in &mut position {
        *value = r.f64().ok()?;
    }
    for _ in 0..3 {
        r.f64().ok()?; // delta, implied by the 26.3 linear path
    }
    let yaw = r.f32().ok()?;
    let pitch = r.f32().ok()?;
    let on_ground = r.bool().ok()?;
    let mut w = Writer::new();
    w.varint(entity);
    w.varint(0); // linear path
    for value in position {
        w.f64(value);
    }
    w.f32(yaw);
    w.f32(pitch);
    w.bool(on_ground);
    Some(w.data)
}

fn remap_commands(rest: &[u8]) -> Option<Vec<u8>> {
    let id = crate::ids::play::clientbound::COMMANDS;
    reencoded(id, rest, |packet| {
        let Clientbound::Commands(commands) = packet else {
            return false;
        };
        for node in &mut commands.nodes {
            let Some((parser, properties)) = &mut node.parser else {
                continue;
            };
            // 26.3-only argument types; 26.2 reads them as a quotable string.
            if matches!(*parser, 55 | 56 | 57 | 59 | 60) {
                *parser = 5;
                *properties = vec![1];
            }
        }
        true
    })
}

fn remap_block_event(rest: &[u8]) -> Option<Vec<u8>> {
    let id = crate::ids::play::clientbound::BLOCK_EVENT;
    reencoded(id, rest, |packet| {
        let Clientbound::BlockEvent(event) = packet else {
            return false;
        };
        event.block = map::to_26_2(&map::BLOCKS, event.block).unwrap_or(1);
        true
    })
}

fn remap_particles(rest: &[u8], to_26_2: bool) -> Option<Vec<u8>> {
    let id = crate::ids::play::clientbound::LEVEL_PARTICLES;
    reencoded(id, rest, |packet| {
        let Clientbound::LevelParticles(particles) = packet else {
            return false;
        };
        let mapped = if to_26_2 {
            map::to_26_2(&map::PARTICLES, particles.particle.id)
        } else {
            map::to_26_3(&map::PARTICLES, particles.particle.id)
        };
        let Some(mapped) = mapped else {
            return false;
        };
        particles.particle.id = mapped;
        if let Some(crate::entity_data::ParticleData::State(state)) = &mut particles.particle.data {
            *state = if to_26_2 {
                map::block_state_to_26_2(*state)
            } else {
                map::block_state_to_26_3(*state)
            };
        }
        true
    })
}

fn remap_sound(id: i32, rest: &[u8], to_26_2: bool) -> Option<Vec<u8>> {
    reencoded(id, rest, |packet| {
        let holder = match packet {
            Clientbound::Sound(sound) => &mut sound.sound,
            Clientbound::SoundEntity(sound) => &mut sound.sound,
            _ => return false,
        };
        if holder.1.is_some() {
            return true;
        }
        let mapped = if to_26_2 {
            map::to_26_2(&map::SOUNDS, holder.0)
        } else {
            map::to_26_3(&map::SOUNDS, holder.0)
        };
        let Some(mapped) = mapped else {
            return false;
        };
        holder.0 = mapped;
        true
    })
}

fn remap_items(id: i32, rest: &[u8], to_26_2: bool) -> Option<Vec<u8>> {
    reencoded(id, rest, |packet| {
        let stack = |slot: &mut Option<crate::item::Stack>| {
            let Some(stack) = slot else {
                return;
            };
            let mapped = if to_26_2 {
                map::to_26_2(&map::ITEMS, stack.item)
            } else {
                map::to_26_3(&map::ITEMS, stack.item)
            };
            let Some(mapped) = mapped else {
                *slot = None;
                return;
            };
            stack.item = mapped;
            if to_26_2 {
                // 26.3's component ids and payloads are not 26.2's; the base
                // stack survives, the patch does not.
                stack.added.clear();
                stack.removed.clear();
            }
        };
        match packet {
            Clientbound::ContainerSetContent(content) => {
                for item in &mut content.items {
                    stack(item);
                }
                stack(&mut content.carried);
            }
            Clientbound::ContainerSetSlot(slot) => stack(&mut slot.item),
            Clientbound::SetCursorItem(cursor) => stack(&mut cursor.item),
            Clientbound::SetPlayerInventory(inventory) => stack(&mut inventory.item),
            Clientbound::SetEquipment(equipment) => {
                for (_, item) in &mut equipment.slots {
                    stack(item);
                }
            }
            _ => return false,
        }
        true
    })
}

fn reencoded(
    id: i32,
    rest: &[u8],
    mutate: impl FnOnce(&mut Clientbound) -> bool,
) -> Option<Vec<u8>> {
    let mut packet = Clientbound::decode(id, rest).ok()?;
    if !mutate(&mut packet) {
        return None;
    }
    encode_body(&packet)
}

fn encode_body(packet: &Clientbound) -> Option<Vec<u8>> {
    let encoded = packet.encode();
    let (_, body) = split(&encoded)?;
    Some(body.to_vec())
}

/// 26.3-only data components shown in a chat item hover. 26.2 cannot parse
/// them; the item itself and its 26.2 components stay.
const NEW_HOVER_COMPONENTS: &[&str] = &[
    "provides_trim_material",
    "provides_pottery_pattern",
    "block_transformer",
    "compostable",
    "interact_animation",
    "attack_animation",
    "trim",
    "cooking_fuel",
    "brewing_fuel",
    "villager_food",
    "mob_visibility",
    "sign_text_back",
    "sign_text_front",
    "waxed",
    "cushion_color",
    "map_decorations",
    "pot_decorations",
];

fn strip_hover_components(tag: &mut Tag) {
    match tag {
        Tag::Compound(map) => {
            for key in ["hover_event", "hoverEvent"] {
                if let Some(Tag::Compound(hover)) = map.get_mut(&key.into()) {
                    let show_item = matches!(
                        hover.get(&"action".into()),
                        Some(Tag::String(action)) if action.to_string_lossy() == "show_item"
                    );
                    if show_item {
                        if let Some(Tag::Compound(components)) = hover.get_mut(&"components".into())
                        {
                            for name in NEW_HOVER_COMPONENTS {
                                components.remove(&(*name).into());
                            }
                        }
                    }
                }
            }
            for value in map.values_mut() {
                strip_hover_components(value);
            }
        }
        Tag::List(list) => {
            for value in list {
                strip_hover_components(value);
            }
        }
        _ => {}
    }
}

fn remap_chat_components(id: i32, rest: &[u8]) -> Option<Vec<u8>> {
    reencoded(id, rest, |packet| {
        match packet {
            Clientbound::PlayerChat(p) => {
                if let Some(tag) = &mut p.unsigned_content {
                    strip_hover_components(tag);
                }
            }
            Clientbound::SystemChat(p) => strip_hover_components(&mut p.content),
            Clientbound::DisguisedChat(p) => strip_hover_components(&mut p.message),
            _ => return false,
        }
        true
    })
}

/// 26.3's only new metadata serializer is `DYE_COLOR` (43); a 26.2 client
/// would stop at it, so those entries are dropped and the rest kept.
fn set_entity_data_to_26_2(rest: &[u8], w: &mut Writer) -> Option<()> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let mut entries = crate::entity_data::EntityData::read(&mut r).ok()?;
    entries.retain(|(_, kind, _)| *kind != crate::entity_data::serializer::DYE_COLOR);
    for (_, serializer, value) in &mut entries {
        remap_metadata(value, *serializer, true);
    }
    w.varint(entity);
    crate::entity_data::EntityData::write(&entries, w);
    Some(())
}

fn set_entity_data_from_26_2(rest: &[u8], w: &mut Writer) -> Option<()> {
    let mut r = Reader::new(rest);
    let entity = r.varint().ok()?;
    let mut entries = crate::entity_data::EntityData::read(&mut r).ok()?;
    for (_, serializer, value) in &mut entries {
        remap_metadata(value, *serializer, false);
    }
    w.varint(entity);
    crate::entity_data::EntityData::write(&entries, w);
    Some(())
}

fn remap_metadata(value: &mut crate::entity_data::Value, serializer: i32, to_26_2: bool) {
    use crate::entity_data::{Particle, ParticleData, Value, serializer};
    match (serializer, value) {
        (serializer::BLOCK_STATE, Value::Id(id)) => {
            *id = if to_26_2 {
                map::block_state_to_26_2(*id)
            } else {
                map::block_state_to_26_3(*id)
            };
        }
        (serializer::OPTIONAL_BLOCK_STATE, Value::Id(id)) if *id != 0 => {
            *id = if to_26_2 {
                map::block_state_to_26_2(*id)
            } else {
                map::block_state_to_26_3(*id)
            };
        }
        (serializer::ITEM_STACK, Value::Item(slot)) => remap_slot(slot, to_26_2),
        (serializer::PARTICLE, Value::Particle(particle)) => remap_particle(particle, to_26_2),
        (serializer::PARTICLES, Value::Particles(particles)) => {
            for particle in particles {
                remap_particle(particle, to_26_2);
            }
        }
        _ => {}
    }

    fn remap_particle(particle: &mut Particle, to_26_2: bool) {
        let mapped = if to_26_2 {
            map::to_26_2(&map::PARTICLES, particle.id)
        } else {
            map::to_26_3(&map::PARTICLES, particle.id)
        };
        if let Some(mapped) = mapped {
            particle.id = mapped;
        }
        if let Some(ParticleData::State(state)) = &mut particle.data {
            *state = if to_26_2 {
                map::block_state_to_26_2(*state)
            } else {
                map::block_state_to_26_3(*state)
            };
        }
    }
}

fn remap_slot(slot: &mut Option<crate::item::Stack>, to_26_2: bool) {
    let Some(stack) = slot else {
        return;
    };
    let mapped = if to_26_2 {
        map::to_26_2(&map::ITEMS, stack.item)
    } else {
        map::to_26_3(&map::ITEMS, stack.item)
    };
    let Some(mapped) = mapped else {
        *slot = None;
        return;
    };
    stack.item = mapped;
    if to_26_2 {
        stack.added.clear();
        stack.removed.clear();
    }
}

/// Skips the join packet up to (not including) the spawn info's game type.
fn login_prefix(r: &mut Reader) -> Option<()> {
    r.i32().ok()?; // player id
    r.bool().ok()?; // hardcore
    let levels = r.varint().ok()?;
    if levels < 0 {
        return None;
    }
    for _ in 0..levels {
        ident(r)?;
    }
    r.varint().ok()?; // max players
    r.varint().ok()?; // chunk radius
    r.varint().ok()?; // simulation distance
    r.bool().ok()?; // reduced debug info
    r.bool().ok()?; // show death screen
    r.bool().ok()?; // limited crafting
    r.varint().ok()?; // dimension type
    ident(r)?;
    r.i64().ok()?; // seed
    Some(())
}

fn ident(r: &mut Reader) -> Option<()> {
    let len = r.varint().ok()?;
    if len < 0 {
        return None;
    }
    r.bytes(len as usize).ok()?;
    Some(())
}

/// 26.3's VarInt game types become 26.2's bytes (255 is "no previous").
fn login_to_26_2(rest: &[u8], w: &mut Writer) -> Option<()> {
    let mut r = Reader::new(rest);
    login_prefix(&mut r)?;
    let prefix = &rest[..r.position()];
    let game = r.varint().ok()?;
    let previous = r.varint().ok()?;
    let tail = r.rest();
    w.bytes(prefix);
    w.u8(u8::try_from(game).ok()?);
    w.u8(if previous == 0 {
        255
    } else {
        u8::try_from(previous - 1).ok()?
    });
    w.bytes(tail);
    Some(())
}

/// 26.2's byte game types become 26.3's VarInts.
fn login_from_26_2(rest: &[u8], w: &mut Writer) -> Option<()> {
    let mut r = Reader::new(rest);
    login_prefix(&mut r)?;
    let prefix = &rest[..r.position()];
    let game = r.u8().ok()?;
    let previous = r.u8().ok()?;
    let tail = r.rest();
    w.bytes(prefix);
    w.varint(i32::from(game));
    w.varint(if previous == 255 {
        0
    } else {
        i32::from(previous) + 1
    });
    w.bytes(tail);
    Some(())
}

/// One 26.3 serverbound play body from this world's own client → a 26.2
/// host's: the inverse of [`rewrite_serverbound_play`]. 26.3's empty punch
/// becomes 26.2's swing of the main hand. One allocation.
pub fn rewrite_serverbound_from_26_3(body: &[u8]) -> Option<Vec<u8>> {
    let (id, rest) = split(body)?;
    let mut w = output(body.len() + 1, serverbound_play_from_26_3(id)?);
    if id == crate::ids::play::serverbound::PUNCH {
        w.u8(0); // main hand
    } else if id == crate::ids::play::serverbound::PLAYER_ACTION {
        w.bytes(&remap_player_action(rest, false)?);
    } else {
        w.bytes(rest);
    }
    Some(w.data)
}

/// One 26.3 clientbound configuration body → the 26.2 guest's. One
/// allocation.
pub fn rewrite_clientbound_configuration(body: &[u8]) -> Option<Vec<u8>> {
    let (id, rest) = split(body)?;
    let target = clientbound_configuration(id)?;
    if id == crate::ids::configuration::clientbound::REGISTRY_DATA {
        use crate::packets::configuration::Clientbound;
        let mut packet = Clientbound::decode(id, rest).ok()?;
        let Clientbound::RegistryData(registry) = &mut packet else {
            return None;
        };
        if !registry_to_26_2(registry) {
            return None;
        }
        let encoded = packet.encode();
        let (_, body) = split(&encoded)?;
        let mut w = output(body.len() + 1, target);
        w.bytes(body);
        return Some(w.data);
    }
    let mut w = output(body.len(), target);
    w.bytes(rest);
    Some(w.data)
}

/// The 26.3 registries 26.2 does not have, and the NBT fields it renamed.
fn registry_to_26_2(registry: &mut crate::packets::configuration::RegistryData) -> bool {
    let key = registry
        .registry
        .strip_prefix("minecraft:")
        .unwrap_or(&registry.registry);
    if matches!(
        key,
        "decorated_pot_pattern" | "block_transformer" | "worldgen/block_state_provider"
    ) {
        return false;
    }
    if key == "trim_material" {
        for entry in &mut registry.entries {
            let Some(Tag::Compound(tag)) = &mut entry.data else {
                continue;
            };
            let Some(Tag::String(palette)) = tag.remove(&JavaString::from("palette_id")) else {
                continue;
            };
            let palette = palette.to_string_lossy();
            let path = palette.strip_prefix("minecraft:").unwrap_or(&palette);
            let asset = path.strip_prefix("trim/").unwrap_or(path);
            tag.insert("asset_name".into(), Tag::String(asset.into()));
        }
    }
    if key == "dimension_type" || key == "worldgen/biome" {
        for entry in &mut registry.entries {
            let Some(Tag::Compound(tag)) = &mut entry.data else {
                continue;
            };
            let Some(Tag::Compound(attributes)) = tag.get_mut(&JavaString::from("attributes"))
            else {
                continue;
            };
            for value in attributes.values_mut() {
                let Tag::Compound(value) = value else {
                    continue;
                };
                let Some(Tag::String(modifier)) = value.get_mut(&JavaString::from("modifier"))
                else {
                    continue;
                };
                if modifier.to_string_lossy() == "append" || modifier.to_string_lossy() == "overlay"
                {
                    *modifier = "override".into();
                }
            }
        }
    }
    true
}

#[cfg(test)]
pub(crate) fn split_for_tests(body: &[u8]) -> Option<(i32, &[u8])> {
    split(body)
}

fn split(body: &[u8]) -> Option<(i32, &[u8])> {
    let mut r = Reader::new(body);
    let id = r.varint().ok()?;
    Some((id, r.rest()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_play_clientbound_ids_shift_after_the_insertions() {
        assert_eq!(clientbound_play(0), Some(0));
        assert_eq!(clientbound_play(36), Some(36));
        assert_eq!(clientbound_play(37), None);
        assert_eq!(clientbound_play(38), Some(37));
        assert_eq!(clientbound_play(82), Some(81));
        assert_eq!(clientbound_play(83), None);
        assert_eq!(clientbound_play(84), Some(82));
        assert_eq!(clientbound_play(122), Some(120));
        assert_eq!(clientbound_play(123), None);
        assert_eq!(clientbound_play(124), Some(121));
        assert_eq!(clientbound_play(143), Some(140));
        assert_eq!(clientbound_play(144), None);
        // Every 26.3 id is forwarded except the three 26.3-only packets.
        let forwarded = (0..=143)
            .filter(|&id| clientbound_play(id).is_some())
            .count();
        assert_eq!(forwarded, 141);
    }

    #[test]
    fn chat_hover_items_lose_only_the_26_3_components() {
        let content = Tag::compound([
            ("text", Tag::String("hello".into())),
            (
                "hover_event",
                Tag::compound([
                    ("action", Tag::String("show_item".into())),
                    (
                        "components",
                        Tag::compound([
                            ("provides_trim_material", Tag::String("x".into())),
                            ("custom_name", Tag::String("keep".into())),
                        ]),
                    ),
                ]),
            ),
        ]);
        let body = Clientbound::SystemChat(crate::packets::play::SystemChat {
            content,
            overlay: false,
        })
        .encode();
        let out = rewrite_clientbound_play(&body).unwrap();
        let (_, rest) = split(&out).unwrap();
        let Clientbound::SystemChat(packet) =
            Clientbound::decode(crate::ids::play::clientbound::SYSTEM_CHAT, rest).unwrap()
        else {
            panic!("not system chat");
        };
        let Tag::Compound(root) = &packet.content else {
            panic!("not a compound");
        };
        let Some(Tag::Compound(hover)) = root.get(&"hover_event".into()) else {
            panic!("hover lost");
        };
        let Some(Tag::Compound(components)) = hover.get(&"components".into()) else {
            panic!("components lost");
        };
        assert!(components.get(&"provides_trim_material".into()).is_none());
        assert!(components.get(&"custom_name".into()).is_some());
    }

    #[test]
    fn recipe_display_packets_are_dropped_in_both_directions() {
        // 26.3 ids for recipe_book_add, place_ghost_recipe and update_recipes.
        for id in [64, 75, 136] {
            assert_eq!(rewrite_clientbound_play(&[id]), None, "26.3 {id}");
        }
        // Their 26.2 ids after the reverse mapping.
        for id in [63, 74, 133] {
            assert_eq!(rewrite_clientbound_from_26_2(&[id]), None, "26.2 {id}");
        }
    }

    #[test]
    fn play_ids_match_the_official_26_2_and_26_3_reports() {
        // Generated with net.minecraft.data.Main --reports from the official
        // 26.2 and 26.3 client jars: 26.2 0..=140 maps in four constant runs.
        for (start, end, delta) in [(0, 36, 0), (37, 81, 1), (82, 120, 2), (121, 140, 3)] {
            for id in start..=end {
                assert_eq!(clientbound_from_26_2(id), Some(id + delta));
                assert_eq!(clientbound_play(id + delta), Some(id));
            }
        }
        for id in [37, 83, 123] {
            assert_eq!(clientbound_play(id), None);
        }
        assert_eq!(clientbound_from_26_2(141), None);
    }

    #[test]
    fn the_play_serverbound_ids_shift_around_punch() {
        assert_eq!(serverbound_play(0), Some(Serverbound::Forward(0)));
        assert_eq!(serverbound_play(45), Some(Serverbound::Forward(45)));
        assert_eq!(serverbound_play(46), Some(Serverbound::Forward(47)));
        assert_eq!(serverbound_play(62), Some(Serverbound::Forward(63)));
        assert_eq!(serverbound_play(63), Some(Serverbound::Punch));
        assert_eq!(serverbound_play(64), Some(Serverbound::Forward(64)));
        assert_eq!(serverbound_play(68), Some(Serverbound::Forward(68)));
        assert_eq!(serverbound_play(69), None);
        // Every 26.2 id forwards; only swing changes packet.
        assert!((0..=68).all(|id| serverbound_play(id).is_some()));
    }

    #[test]
    fn bodies_keep_their_payload_and_swing_becomes_an_empty_punch() {
        assert_eq!(
            rewrite_clientbound_play(&[38, 0xAA, 0xBB]),
            Some(vec![37, 0xAA, 0xBB])
        );
        assert_eq!(rewrite_clientbound_play(&[37, 0xAA]), None);
        assert_eq!(rewrite_serverbound_play(&[46, 0xAA]), Some(vec![47, 0xAA]));
        assert_eq!(rewrite_serverbound_play(&[63, 0]), Some(vec![46]));
        assert_eq!(
            rewrite_clientbound_from_26_2(&[37, 0xAA]),
            Some(vec![38, 0xAA])
        );
        assert_eq!(rewrite_clientbound_from_26_2(&[36]), Some(vec![36]));
        // 140 and 143 are two-byte VarInts.
        assert_eq!(
            rewrite_clientbound_from_26_2(&[0x8C, 0x01]),
            Some(vec![0x8F, 0x01])
        );
        // swing_animation (123) becomes animate (2), not a shifted id.
        assert_eq!(
            rewrite_clientbound_play(&[123, 5, 0, 1]),
            Some(vec![2, 5, 0])
        );
        assert_eq!(rewrite_clientbound_play(&[123, 5, 0, 0]), None);
        assert_eq!(
            rewrite_serverbound_from_26_3(&[47, 0xAA]),
            Some(vec![46, 0xAA])
        );
        assert_eq!(rewrite_serverbound_from_26_3(&[46]), Some(vec![63, 0]));
        assert_eq!(
            rewrite_clientbound_configuration(&[11, 0xAA]),
            Some(vec![10, 0xAA])
        );
        assert_eq!(rewrite_clientbound_configuration(&[10]), None);
        assert_eq!(rewrite_clientbound_play(&[]), None);
    }

    fn login_26_3() -> Vec<u8> {
        let mut w = Writer::new();
        w.i32(42);
        w.bool(false);
        w.varint(1);
        w.varint(19);
        w.bytes(b"minecraft:overworld");
        w.varint(20);
        w.varint(10);
        w.varint(10);
        w.bool(false);
        w.bool(true);
        w.bool(false);
        w.varint(0);
        w.varint(19);
        w.bytes(b"minecraft:overworld");
        w.i64(0);
        w.varint(1); // game type
        w.varint(0); // no previous game type
        w.bool(false); // debug
        w.bool(false); // flat
        w.bool(false); // no last death
        w.varint(0); // portal cooldown
        w.varint(63); // sea level
        w.bool(false); // online mode
        w.bool(false); // secure chat
        w.data
    }

    #[test]
    fn the_join_packet_rewrites_game_types_and_round_trips() {
        let mut body = Writer::new();
        body.varint(crate::ids::play::clientbound::LOGIN);
        body.bytes(&login_26_3());
        let to_26_2 = rewrite_clientbound_play(&body.data).unwrap();
        let mut r = Reader::new(&to_26_2);
        assert_eq!(
            r.varint().unwrap(),
            crate::ids::play::clientbound::LOGIN - 1
        );
        let mut rest = Reader::new(r.rest());
        login_prefix(&mut rest).unwrap();
        assert_eq!(rest.u8().unwrap(), 1, "game type is one byte");
        assert_eq!(rest.u8().unwrap(), 255, "no previous game type");
        let back = rewrite_clientbound_from_26_2(&to_26_2).unwrap();
        assert_eq!(back, body.data);
    }

    #[test]
    fn entity_metadata_drops_the_26_3_only_dye_colour() {
        let mut body = Writer::new();
        body.varint(crate::ids::play::clientbound::SET_ENTITY_DATA);
        body.varint(7);
        crate::entity_data::EntityData::write(
            &vec![
                (
                    0,
                    crate::entity_data::serializer::BYTE,
                    crate::entity_data::Value::Byte(2),
                ),
                (
                    4,
                    crate::entity_data::serializer::DYE_COLOR,
                    crate::entity_data::Value::Id(1),
                ),
                (
                    5,
                    crate::entity_data::serializer::POSE,
                    crate::entity_data::Value::Id(0),
                ),
            ],
            &mut body,
        );
        let rewritten = rewrite_clientbound_play(&body.data).unwrap();
        let mut r = Reader::new(&rewritten);
        assert_eq!(
            r.varint().unwrap(),
            clientbound_play(crate::ids::play::clientbound::SET_ENTITY_DATA).unwrap()
        );
        assert_eq!(r.varint().unwrap(), 7);
        let entries = crate::entity_data::EntityData::read(&mut r).unwrap();
        assert_eq!(
            entries.iter().map(|(index, ..)| *index).collect::<Vec<_>>(),
            vec![0, 5]
        );
    }

    #[test]
    fn every_id_round_trips_through_the_other_release() {
        for id in 0..=143 {
            match clientbound_play(id) {
                Some(mapped) => assert_eq!(clientbound_from_26_2(mapped), Some(id), "{id}"),
                None => assert!(
                    matches!(id, 37 | 83 | 123),
                    "only the three 26.3-only packets are dropped"
                ),
            }
        }
        for id in 0..=68 {
            let mapped = match serverbound_play(id).unwrap() {
                Serverbound::Forward(id) => id,
                Serverbound::Punch => crate::ids::play::serverbound::PUNCH,
            };
            assert_eq!(serverbound_play_from_26_3(mapped), Some(id), "{id}");
        }
    }

    #[test]
    fn the_configuration_ids_shift_after_post_effects() {
        assert_eq!(clientbound_configuration(0), Some(0));
        assert_eq!(clientbound_configuration(9), Some(9));
        assert_eq!(clientbound_configuration(10), None);
        assert_eq!(clientbound_configuration(11), Some(10));
        assert_eq!(clientbound_configuration(20), Some(19));
        assert_eq!(clientbound_configuration(21), None);
        for id in 0..=9 {
            assert_eq!(serverbound_configuration(id), Some(id));
        }
        assert_eq!(serverbound_configuration(10), None);
    }

    fn add_entity(kind: i32) -> Vec<u8> {
        use crate::packets::play::AddEntity;
        Clientbound::AddEntity(AddEntity {
            id: 5,
            uuid: 0,
            kind,
            position: [0.0; 3],
            velocity: [0.0; 3],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 0,
        })
        .encode()
    }

    fn add_entity_kind(body: &[u8]) -> i32 {
        let (_, rest) = split(body).unwrap();
        let mut r = Reader::new(rest);
        r.varint().unwrap();
        r.bytes(16).unwrap();
        r.varint().unwrap()
    }

    #[test]
    fn remaps_entity_kinds_in_both_directions() {
        let host = rewrite_clientbound_play(&add_entity(34)).unwrap();
        assert_eq!(add_entity_kind(&host), 33);
        // A 26.3-only type has no 26.2 id and is not sent.
        assert!(rewrite_clientbound_play(&add_entity(33)).is_none());
        let guest = rewrite_clientbound_from_26_2(&add_entity(33)).unwrap();
        assert_eq!(add_entity_kind(&guest), 34);
    }

    fn block_update(state: i32) -> Vec<u8> {
        use crate::packets::play::BlockUpdate;
        Clientbound::BlockUpdate(BlockUpdate {
            pos: [1, 2, 3],
            state,
        })
        .encode()
    }

    fn block_update_state(body: &[u8]) -> i32 {
        let (_, rest) = split(body).unwrap();
        let mut r = Reader::new(rest);
        r.i64().unwrap();
        r.varint().unwrap()
    }

    #[test]
    fn remaps_block_states_in_updates_and_chunks() {
        let host = rewrite_clientbound_play(&block_update(28)).unwrap();
        assert_eq!(block_update_state(&host), 27);
        let guest = rewrite_clientbound_from_26_2(&block_update(27)).unwrap();
        assert_eq!(block_update_state(&guest), 28);

        use crate::{
            chunk::{BLOCKS, ChunkData, LightData, Paletted, Section, Strategy},
            packets::play::LevelChunkWithLight,
        };
        let section = |state| Section {
            non_empty: 1,
            fluids: 0,
            states: Paletted::from_values(&vec![state; BLOCKS], Strategy::Blocks, 0),
            biomes: Paletted::from_values(&vec![0; 64], Strategy::Biomes, 0),
        };
        let body = |state| {
            Clientbound::LevelChunkWithLight(LevelChunkWithLight {
                x: 0,
                z: 0,
                chunk: ChunkData {
                    heightmaps: Vec::new(),
                    buffer: ChunkData::from_sections(&[section(state)]),
                    block_entities: Vec::new(),
                },
                light: LightData {
                    sky_mask: Vec::new(),
                    block_mask: Vec::new(),
                    empty_sky_mask: Vec::new(),
                    empty_block_mask: Vec::new(),
                    sky: Vec::new(),
                    block: Vec::new(),
                },
            })
            .encode()
        };
        for (from, to, host) in [(28, 27, true), (27, 28, false)] {
            let body = body(from);
            let body = if host {
                body
            } else {
                // The guest side receives 26.2's id for the chunk packet.
                let (_, rest) = split(&body).unwrap();
                let mut w = Writer::new();
                w.varint(
                    clientbound_play(crate::ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT)
                        .unwrap(),
                );
                w.bytes(rest);
                w.data
            };
            let out = if host {
                rewrite_clientbound_play(&body)
            } else {
                rewrite_clientbound_from_26_2(&body)
            }
            .unwrap();
            let (_, rest) = split(&out).unwrap();
            let packet =
                Clientbound::decode(crate::ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT, rest)
                    .unwrap();
            let Clientbound::LevelChunkWithLight(chunk) = packet else {
                panic!("not a chunk");
            };
            let sections = chunk.chunk.all_sections().unwrap();
            assert_eq!(sections[0].states.get(0), to);
        }
    }

    #[test]
    fn remaps_item_ids_and_drops_26_3_components() {
        use crate::{
            item::{Component, Stack},
            packets::play::ContainerSetSlot,
        };
        let mut stack = Stack::new(73, 1);
        stack.added.push((3, Component::Int(9)));
        let body = Clientbound::ContainerSetSlot(ContainerSetSlot {
            container: 0,
            state: 0,
            slot: 0,
            item: Some(stack),
        })
        .encode();
        let out = rewrite_clientbound_play(&body).unwrap();
        let (id, rest) = split(&out).unwrap();
        let Clientbound::ContainerSetSlot(slot) = Clientbound::decode(id, rest).unwrap() else {
            panic!("not a slot");
        };
        let stack = slot.item.unwrap();
        assert_eq!(stack.item, 72);
        assert!(stack.added.is_empty());

        let body = Clientbound::ContainerSetSlot(ContainerSetSlot {
            container: 0,
            state: 0,
            slot: 0,
            item: Some(Stack::new(72, 1)),
        })
        .encode();
        let out = rewrite_clientbound_from_26_2(&body).unwrap();
        let (id, rest) = split(&out).unwrap();
        let Clientbound::ContainerSetSlot(slot) = Clientbound::decode(id, rest).unwrap() else {
            panic!("not a slot");
        };
        assert_eq!(slot.item.unwrap().item, 73);
    }

    #[test]
    fn remaps_metadata_item_block_and_particle_ids() {
        use crate::{
            entity_data::{Value as Meta, serializer},
            packets::play::SetEntityData,
        };
        let body = Clientbound::SetEntityData(SetEntityData {
            id: 5,
            values: vec![
                (0, serializer::BLOCK_STATE, Meta::Id(28)),
                (
                    1,
                    serializer::ITEM_STACK,
                    Meta::Item(Some(crate::item::Stack::new(73, 1))),
                ),
            ],
        })
        .encode();
        let out = rewrite_clientbound_play(&body).unwrap();
        let (_, rest) = split(&out).unwrap();
        let Clientbound::SetEntityData(data) =
            Clientbound::decode(crate::ids::play::clientbound::SET_ENTITY_DATA, rest).unwrap()
        else {
            panic!("not entity data");
        };
        assert_eq!(data.values[0].2, Meta::Id(27));
        let Meta::Item(Some(stack)) = &data.values[1].2 else {
            panic!("not an item");
        };
        assert_eq!(stack.item, 72);
    }

    #[test]
    fn remaps_entity_position_sync_paths() {
        use crate::packets::play::{EntityPositionSync, PositionPath};
        let body = Clientbound::EntityPositionSync(EntityPositionSync {
            id: 5,
            path: PositionPath::Linear([1.0, 2.0, 3.0]),
            yaw: 90.0,
            pitch: 10.0,
            on_ground: true,
        })
        .encode();
        let out = rewrite_clientbound_play(&body).unwrap();
        let (id, rest) = split(&out).unwrap();
        assert_eq!(id, 35);
        let mut r = Reader::new(rest);
        assert_eq!(r.varint().unwrap(), 5);
        for expected in [1.0, 2.0, 3.0, 0.0, 0.0, 0.0] {
            assert_eq!(r.f64().unwrap(), expected);
        }
        assert_eq!(r.f32().unwrap(), 90.0);
        assert_eq!(r.f32().unwrap(), 10.0);
        assert!(r.bool().unwrap());

        let mut body = Writer::new();
        body.varint(35);
        body.varint(5);
        for value in [1.0, 2.0, 3.0, 0.0, 0.0, 0.0] {
            body.f64(value);
        }
        body.f32(90.0);
        body.f32(10.0);
        body.bool(true);
        let out = rewrite_clientbound_from_26_2(&body.data).unwrap();
        let (_, rest) = split(&out).unwrap();
        let mut r = Reader::new(rest);
        assert_eq!(r.varint().unwrap(), 5);
        assert_eq!(r.varint().unwrap(), 0); // linear path
        for expected in [1.0, 2.0, 3.0] {
            assert_eq!(r.f64().unwrap(), expected);
        }
        assert_eq!(r.f32().unwrap(), 90.0);
        assert_eq!(r.f32().unwrap(), 10.0);
        assert!(r.bool().unwrap());
    }

    #[test]
    fn remaps_26_3_only_command_arguments() {
        use crate::packets::play::{CommandNode, Commands};
        let body = Clientbound::Commands(Commands {
            nodes: vec![CommandNode {
                kind: 2,
                executable: false,
                restricted: false,
                children: Vec::new(),
                redirect: None,
                name: "x".into(),
                parser: Some((55, Vec::new())),
                suggestions: None,
            }],
            root: 0,
        })
        .encode();
        let out = rewrite_clientbound_play(&body).unwrap();
        let (_, rest) = split(&out).unwrap();
        let Clientbound::Commands(commands) =
            Clientbound::decode(crate::ids::play::clientbound::COMMANDS, rest).unwrap()
        else {
            panic!("not commands");
        };
        assert_eq!(commands.nodes[0].parser, Some((5, vec![1])));
    }

    #[test]
    fn remaps_the_entity_movement_formats() {
        let mut body = Writer::new();
        body.varint(54);
        body.varint(5); // entity
        body.varint(1); // on ground, no steps
        body.i16(1);
        body.i16(2);
        body.i16(3);
        let out = rewrite_clientbound_play(&body.data).unwrap();
        let (id, rest) = split(&out).unwrap();
        assert_eq!(id, 53);
        let mut r = Reader::new(rest);
        assert_eq!(r.varint().unwrap(), 5);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 2);
        assert_eq!(r.i16().unwrap(), 3);
        assert!(r.bool().unwrap());

        let mut body = Writer::new();
        body.varint(53);
        body.varint(5);
        body.i16(1);
        body.i16(2);
        body.i16(3);
        body.bool(true);
        let out = rewrite_clientbound_from_26_2(&body.data).unwrap();
        let (id, rest) = split(&out).unwrap();
        assert_eq!(id, 54);
        let mut r = Reader::new(rest);
        assert_eq!(r.varint().unwrap(), 5);
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 2);
        assert_eq!(r.i16().unwrap(), 3);
    }

    #[test]
    fn rewrites_26_3_registry_data_for_26_2() {
        use crate::packets::configuration::{Clientbound as Config, RegistryData, RegistryEntry};
        let body = Config::RegistryData(RegistryData {
            registry: "minecraft:trim_material".into(),
            entries: vec![RegistryEntry {
                id: "minecraft:quartz".into(),
                data: Some(Tag::compound([(
                    "palette_id",
                    Tag::String("minecraft:trim/quartz".into()),
                )])),
            }],
        })
        .encode();
        let out = rewrite_clientbound_configuration(&body).unwrap();
        let (id, rest) = split(&out).unwrap();
        assert_eq!(id, 7);
        let Config::RegistryData(data) = Config::decode(id, rest).unwrap() else {
            panic!("not registry data");
        };
        let Some(Tag::Compound(tag)) = &data.entries[0].data else {
            panic!("no tag");
        };
        assert_eq!(
            tag.get(&JavaString::from("asset_name"))
                .and_then(|tag| match tag {
                    Tag::String(value) => Some(value.to_string_lossy()),
                    _ => None,
                }),
            Some("quartz".to_string())
        );
        assert!(tag.get(&JavaString::from("palette_id")).is_none());

        let body = Config::RegistryData(RegistryData {
            registry: "minecraft:block_transformer".into(),
            entries: Vec::new(),
        })
        .encode();
        assert!(rewrite_clientbound_configuration(&body).is_none());
    }
}
