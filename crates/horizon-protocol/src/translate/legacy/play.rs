//! The world's play packets as protocol 47's, where 1.8 has an equivalent.
//! Packets 1.8 has no counterpart for (entities, items, inventories, the
//! modern configuration) are dropped; the guest sees the world and its own
//! movement, chat and health.
use super::{LegacyState, chunk_data_body, packets, section, states_of};
use crate::{
    buf::Reader,
    chunk::{ChunkData, LightData},
    packets::play::{self, Clientbound},
};
use horizon_world::nbt::Tag;

/// What a modern chunk packet needs to be read and translated: the level's
/// block sections, where y = 0 sits among them, and the bits the block and
/// biome registries take in a global palette.
#[derive(Clone, Copy, Debug)]
pub struct ChunkShape {
    pub sections: usize,
    /// The modern section index that holds y = 0 (4 for a world from -64).
    pub first_1_8: usize,
    pub block_bits: u8,
    pub biome_bits: u8,
    pub min_y: i32,
    pub sky_light: bool,
}

impl ChunkShape {
    /// A level's shape from its build range and registries.
    pub fn new(
        sections: usize,
        min_y: i32,
        block_states: usize,
        biomes: usize,
        sky_light: bool,
    ) -> Self {
        let bits = |n: usize| (usize::BITS - n.saturating_sub(1).leading_zeros()) as u8;
        Self {
            sections,
            first_1_8: usize::try_from((-min_y).max(0) / 16)
                .unwrap_or(0)
                .min(sections),
            block_bits: bits(block_states),
            biome_bits: bits(biomes),
            min_y,
            sky_light,
        }
    }

    /// The vanilla overworld: 24 sections from -64 to 319, y = 0 in the
    /// fifth.
    pub fn overworld(block_states: usize, biomes: usize, sky_light: bool) -> Self {
        Self::new(24, -64, block_states, biomes, sky_light)
    }
}

/// A `LevelChunkWithLight` as a 1.8 `Chunk Data` body: the 16 sections that
/// cover y 0–255, their block and sky light, and a column of biomes from the
/// surface section's cells. Sections the modern chunk does not have in that
/// range, and empty ones, leave their mask bit clear.
pub fn chunk_data(
    x: i32,
    z: i32,
    chunk: &ChunkData,
    light: &LightData,
    shape: ChunkShape,
    resolve: impl Fn(u32) -> LegacyState,
    biome: impl Fn(u32) -> u8,
) -> Option<Vec<u8>> {
    let sections = chunk
        .sections(shape.sections, shape.block_bits, shape.biome_bits)
        .ok()?;
    let layers = light.layers(shape.sections + 2);
    let mut mask = 0u16;
    let mut bodies = Vec::new();
    for local in 0..16usize {
        let index = shape.first_1_8 + local;
        let Some(part) = sections.get(index) else {
            continue;
        };
        if part.non_empty <= 0 {
            continue;
        }
        mask |= 1 << local;
        let states = states_of(|i| part.states.get(i) as u32, &resolve);
        let (sky, block) = layers.get(index + 1).cloned().unwrap_or((None, None));
        let block = layer(block.as_ref(), 0);
        let sky = shape.sky_light.then(|| layer(sky.as_ref(), 0xFF));
        bodies.push(section(&states, &block, sky.as_ref()));
    }
    // 1.8 has one biome per column: the surface section's 4×4 cells, each
    // covering four blocks.
    let mut biomes = [1u8; 256];
    let surface = usize::try_from((64 - shape.min_y).max(0) / 16)
        .unwrap_or(0)
        .min(shape.sections.saturating_sub(1));
    if let Some(part) = sections.get(surface) {
        for z in 0..16usize {
            for x in 0..16usize {
                let cell = (z / 4) * 4 + x / 4;
                biomes[z * 16 + x] = biome(part.biomes.get(cell) as u32);
            }
        }
    }
    Some(chunk_data_body(x, z, true, mask, &bodies, Some(&biomes)))
}

/// A 1.8 light array, zero-padded or cut to 2048 bytes.
fn layer(bytes: Option<&Vec<u8>>, default: u8) -> [u8; 2048] {
    let mut out = [default; 2048];
    if let Some(bytes) = bytes {
        let n = bytes.len().min(2048);
        out[..n].copy_from_slice(&bytes[..n]);
    }
    out
}

/// Translates the world's play packets for one 1.8 guest. It keeps the game
/// mode the join game carried, which a later respawn repeats.
#[derive(Default)]
pub struct PlayTranslator {
    game_mode: i32,
    /// A chunk packet's level shape, once the host knows it.
    shape: Option<ChunkShape>,
    /// A network biome id as 1.8's; plains when the host gave none.
    biomes: Option<std::sync::Arc<dyn Fn(u32) -> u8 + Send + Sync>>,
    /// A network entity type id as its name, for the 1.8 spawn packets.
    entities: Option<std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>>,
    /// A network item id as its name, for 1.8 stacks.
    items: Option<std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>>,
}

impl PlayTranslator {
    /// The level's shape, for translating its chunks.
    pub fn set_chunk_shape(&mut self, shape: ChunkShape) {
        self.shape = Some(shape);
    }

    /// The world's biomes by network id, for translating its chunks.
    pub fn set_biomes(&mut self, biomes: std::sync::Arc<dyn Fn(u32) -> u8 + Send + Sync>) {
        self.biomes = Some(biomes);
    }

    /// The world's entity types by network id, for its spawn packets.
    pub fn set_entities(
        &mut self,
        entities: std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>,
    ) {
        self.entities = Some(entities);
    }

    /// The world's item types by network id, for 1.8 stacks.
    pub fn set_items(
        &mut self,
        items: std::sync::Arc<dyn Fn(u32) -> Option<String> + Send + Sync>,
    ) {
        self.items = Some(items);
    }

    /// A 1.8 slot payload: `short -1` for empty, else id, count, damage and an
    /// empty NBT tag.
    fn legacy_stack(&self, stack: Option<&crate::item::Stack>) -> Vec<u8> {
        let empty = vec![0xFF, 0xFF];
        let Some(stack) = stack else {
            return empty;
        };
        let Some(name) = self
            .items
            .as_ref()
            .and_then(|items| items(stack.item as u32))
        else {
            return empty;
        };
        let Some(item) = super::items::item(&name) else {
            return empty;
        };
        let count = stack.count.clamp(0, 255) as u8;
        if count == 0 {
            return empty;
        }
        let damage = stack.damage();
        let metadata = if super::items::durable(item.id) {
            0
        } else {
            i16::try_from(damage).unwrap_or(0)
        };
        let mut w = crate::buf::Writer::new();
        w.i16(item.id);
        w.u8(count);
        w.i16(metadata);
        if super::items::durable(item.id) && damage > 0 {
            // A 1.8 item's NBT is a named compound with an empty name; a
            // damageable item carries its wear in `Damage`.
            w.u8(0x0a);
            w.u16(0);
            w.u8(0x03);
            w.u16(6);
            w.data.extend_from_slice(b"Damage");
            w.i32(damage);
            w.u8(0);
        } else {
            w.u8(0);
        }
        w.data
    }

    /// The body a 1.8 guest gets for a modern play packet body (id and
    /// payload), or `None` when 1.8 has no equivalent here.
    pub fn translate(
        &mut self,
        body: &[u8],
        state: impl Fn(u32) -> LegacyState,
    ) -> Option<Vec<u8>> {
        let mut r = Reader::new(body);
        let id = r.varint().ok()?;
        Some(match Clientbound::decode(id, r.rest()).ok()? {
            Clientbound::KeepAlive(keep) => packets::keep_alive(keep.id as i32),
            Clientbound::Login(login) => {
                self.game_mode = login.spawn.game_type;
                packets::join_game(
                    login.player_id,
                    gamemode(login.spawn.game_type),
                    dimension(&login.spawn.dimension),
                    2,
                    u8::try_from(login.max_players.clamp(0, 255)).unwrap_or(255),
                    "default",
                    login.reduced_debug_info,
                )
            }
            Clientbound::SystemChat(chat) => {
                packets::chat(&text_json(&chat.content), if chat.overlay { 2 } else { 1 })
            }
            Clientbound::SetTime(time) => packets::time_update(
                time.game_time,
                time.clocks.first().map_or(0, |clock| clock.total_ticks),
            ),
            Clientbound::PlayerAbilities(abilities) => packets::player_abilities(
                abilities.flags,
                abilities.flying_speed,
                abilities.walking_speed,
            ),
            Clientbound::SetHealth(health) => {
                packets::update_health(health.health, health.food, health.saturation)
            }
            Clientbound::Respawn(respawn) => {
                self.game_mode = respawn.spawn.game_type;
                packets::respawn(
                    i32::from(dimension(&respawn.spawn.dimension)),
                    2,
                    gamemode(self.game_mode),
                    "default",
                )
            }
            Clientbound::RemoveEntities(remove) => packets::destroy_entities(&remove.ids),
            Clientbound::ContainerSetSlot(slot) => {
                let item = self.legacy_stack(slot.item.as_ref());
                packets::set_slot(window(slot.container), slot.slot, &item)
            }
            Clientbound::ContainerSetContent(content) => {
                let items: Vec<Vec<u8>> = content
                    .items
                    .iter()
                    .map(|stack| self.legacy_stack(stack.as_ref()))
                    .collect();
                let carried = self.legacy_stack(content.carried.as_ref());
                packets::window_items(window(content.container), &items, &carried)
            }
            Clientbound::SetCursorItem(cursor) => {
                let item = self.legacy_stack(cursor.item.as_ref());
                packets::set_slot(0, -1, &item)
            }
            Clientbound::SetPlayerInventory(inventory) => {
                let slot = player_slot(inventory.slot)?;
                let item = self.legacy_stack(inventory.item.as_ref());
                packets::set_slot(0, slot, &item)
            }
            Clientbound::SetEquipment(equipment) => {
                let mut entries = Vec::new();
                for (slot, item) in &equipment.slots {
                    let Some(slot) = equipment_slot(*slot) else {
                        continue;
                    };
                    entries.push((slot, self.legacy_stack(item.as_ref())));
                }
                if entries.is_empty() {
                    return None;
                }
                packets::entity_equipment(equipment.id, &entries)
            }
            Clientbound::ContainerSetData(data) => {
                packets::window_property(window(data.container), data.id, data.value)
            }
            Clientbound::ContainerClose(close) => packets::close_window(window(close.container)),
            Clientbound::MoveEntityPos(m) => {
                let delta = relative(m.0.delta.as_ref()?)?;
                packets::rel_entity_move(m.0.id, delta, m.0.on_ground)
            }
            Clientbound::MoveEntityPosRot(m) => {
                let delta = relative(m.0.delta.as_ref()?)?;
                let (yaw, pitch) = m.0.rotation?;
                packets::entity_look_and_rel_move(m.0.id, delta, yaw, pitch, m.0.on_ground)
            }
            Clientbound::MoveEntityRot(m) => {
                let (yaw, pitch) = m.0.rotation?;
                packets::entity_look(m.0.id, yaw, pitch, m.0.on_ground)
            }
            Clientbound::RotateHead(head) => packets::entity_head_look(head.id, head.head_yaw),
            Clientbound::SetEntityMotion(motion) => {
                packets::entity_velocity(motion.id, motion.velocity)
            }
            Clientbound::TeleportEntity(teleport) => packets::entity_teleport(
                teleport.id,
                teleport.position,
                teleport.yaw,
                teleport.pitch,
                teleport.on_ground,
            ),
            Clientbound::AddEntity(entity) => {
                let name = self.entities.as_ref()?.as_ref()(entity.kind as u32)?;
                match super::entities::spawn_kind(&name)? {
                    super::entities::Spawn::Mob(kind) => packets::spawn_mob(
                        entity.id,
                        kind,
                        entity.position,
                        [entity.yaw, entity.pitch, entity.head_yaw],
                        entity.velocity,
                    ),
                    super::entities::Spawn::Object(kind) => packets::spawn_object(
                        entity.id,
                        kind,
                        entity.position,
                        [entity.yaw, entity.pitch],
                        0,
                        entity.velocity,
                    ),
                }
            }
            Clientbound::SetHeldSlot(held) => {
                packets::held_item_change(i8::try_from(held.slot.clamp(0, 8)).unwrap_or(0))
            }
            Clientbound::LevelChunkWithLight(chunk) => {
                let shape = self.shape?;
                let biomes = self.biomes.clone();
                return chunk_data(
                    chunk.x,
                    chunk.z,
                    &chunk.chunk,
                    &chunk.light,
                    shape,
                    &state,
                    |id| biomes.as_ref().map_or(1, |biomes| biomes(id)),
                );
            }
            Clientbound::BlockUpdate(change) => {
                // 1.8's world ends at y = 255.
                if !(0..=255).contains(&change.pos[1]) {
                    return None;
                }
                packets::block_change(
                    change.pos[0],
                    change.pos[1],
                    change.pos[2],
                    state(change.state as u32),
                )
            }
            Clientbound::SectionBlocksUpdate(update) => {
                // 1.8 has 16 sections, from y = 0.
                if !(0..=15).contains(&update.section[1]) {
                    return None;
                }
                let changes: Vec<([u8; 3], LegacyState)> = update
                    .changes
                    .iter()
                    .map(|&change| {
                        let x = ((change >> 8) & 15) as u8;
                        let z = ((change >> 4) & 15) as u8;
                        let y = (change & 15) as u8;
                        ([x, y, z], state((change >> 12) as u32))
                    })
                    .collect();
                packets::multi_block_change(update.section[0], update.section[2], &changes)
            }
            _ => return None,
        })
    }
}

/// A 1.8 guest's play packets as the world's protocol.
#[derive(Default)]
pub struct GuestTranslator;

impl GuestTranslator {
    /// The modern body for a 1.8 guest packet body, or `None` when the host
    /// has no mapping yet (signed chat, settings, digging, inventories...).
    pub fn from_guest(&mut self, body: &[u8]) -> Option<Vec<u8>> {
        use super::ids::play::serverbound as guest;
        use crate::packets::play::Serverbound;
        let mut r = Reader::new(body);
        let id = r.varint().ok()?;
        let payload = r.rest();
        Some(match id {
            guest::KEEP_ALIVE => {
                let keep = packets::read_keep_alive(id, payload).ok()?;
                Serverbound::KeepAlive(crate::packets::configuration::KeepAlive {
                    id: i64::from(keep),
                })
                .encode()
            }
            guest::PLAYER
            | guest::PLAYER_POSITION
            | guest::PLAYER_LOOK
            | guest::PLAYER_POSITION_AND_LOOK => {
                let movement = packets::read_movement(id, payload).ok()??;
                let flags = u8::from(movement.on_ground);
                match (movement.position, movement.look) {
                    (Some((x, y, z)), Some((yaw, pitch))) => {
                        Serverbound::MovePlayerPosRot(play::MovePlayerPosRot {
                            x,
                            y,
                            z,
                            yaw,
                            pitch,
                            flags,
                        })
                        .encode()
                    }
                    (Some((x, y, z)), None) => {
                        Serverbound::MovePlayerPos(play::MovePlayerPos { x, y, z, flags }).encode()
                    }
                    (None, Some((yaw, pitch))) => {
                        Serverbound::MovePlayerRot(play::MovePlayerRot { yaw, pitch, flags })
                            .encode()
                    }
                    (None, None) => {
                        Serverbound::MovePlayerStatus(play::MovePlayerStatus { flags }).encode()
                    }
                }
            }
            guest::HELD_ITEM_CHANGE => {
                let slot = packets::read_held_item(id, payload).ok()?;
                Serverbound::SetCarriedItem(play::SetCarriedItem {
                    slot: i16::try_from(slot).unwrap_or(0),
                })
                .encode()
            }
            _ => return None,
        })
    }
}

/// A modern container id as 1.8's window id.
fn window(container: i32) -> i8 {
    container.clamp(0, 127) as i8
}

/// A modern equipment slot ordinal (mainhand, offhand, feet, legs, chest,
/// head, body) as 1.8's (held, boots, leggings, chestplate, helmet).
fn equipment_slot(slot: u8) -> Option<i16> {
    match slot {
        0 => Some(0),
        2 => Some(1),
        3 => Some(2),
        4 => Some(3),
        5 => Some(4),
        _ => None,
    }
}

/// A 26.2 Inventory index (hotbar 0–8, main 9–35, feet–head 36–39) as the
/// slot of 1.8's player window (hotbar 36–44, main 9–35, head–feet 5–8).
fn player_slot(slot: i32) -> Option<i16> {
    match slot {
        0..=8 => Some(36 + slot as i16),
        9..=35 => Some(slot as i16),
        36..=39 => Some(44 - slot as i16),
        _ => None,
    }
}

/// A modern relative delta (1/4096 block) as 1.8's (1/32 block), or `None`
/// when it cannot fit in 1.8's byte or is a stepped path.
fn relative(delta: &crate::packets::play::VecDelta) -> Option<[i8; 3]> {
    let crate::packets::play::VecDelta::Linear(delta) = delta else {
        return None;
    };
    let mut out = [0i8; 3];
    for (i, value) in delta.iter().enumerate() {
        let scaled = (f64::from(*value) / 128.0).round();
        if !(-128.0..=127.0).contains(&scaled) {
            return None;
        }
        out[i] = scaled as i8;
    }
    Some(out)
}

/// 1.8's game mode numbers are the modern ones; hardcore is a flag it does
/// not get from here.
fn gamemode(mode: i32) -> u8 {
    match mode {
        1..=3 => mode as u8,
        _ => 0,
    }
}

/// A dimension key as 1.8's number: the Nether is -1, the End 1, the rest 0.
fn dimension(name: &str) -> i8 {
    match name.rsplit(':').next().unwrap_or(name) {
        "the_nether" => -1,
        "the_end" => 1,
        _ => 0,
    }
}

/// A text component (`Tag`) as the JSON string protocol 47 carries.
fn text_json(tag: &Tag) -> String {
    nbt_json(tag).to_string()
}

fn nbt_json(tag: &Tag) -> serde_json::Value {
    use serde_json::Value;
    match tag {
        Tag::End => Value::Null,
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(bits) => number(f64::from(f32::from_bits(*bits))),
        Tag::Double(bits) => number(f64::from_bits(*bits)),
        Tag::ByteArray(bytes) => Value::Array(bytes.iter().map(|v| Value::from(*v)).collect()),
        Tag::String(text) => Value::String(text.to_string_lossy()),
        Tag::List(items) => Value::Array(items.iter().map(nbt_json).collect()),
        Tag::Compound(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.to_string_lossy(), nbt_json(value)))
                .collect(),
        ),
        Tag::IntArray(values) => Value::Array(values.iter().map(|v| Value::from(*v)).collect()),
        Tag::LongArray(values) => Value::Array(values.iter().map(|v| Value::from(*v)).collect()),
    }
}

fn number(value: f64) -> serde_json::Value {
    serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buf::Reader,
        packets::{configuration::KeepAlive, play::*},
        translate::legacy::packets::read_position,
    };

    fn stone(_state: u32) -> LegacyState {
        LegacyState { id: 1, data: 0 }
    }

    #[test]
    fn spawns_a_zombie_as_a_1_8_mob() {
        let mut translator = PlayTranslator::default();
        translator.set_entities(std::sync::Arc::new(|id| {
            (id == 1).then(|| "minecraft:zombie".to_string())
        }));
        let body = Clientbound::AddEntity(AddEntity {
            id: 7,
            uuid: 0,
            kind: 1,
            position: [1.5, 64.0, -2.0],
            velocity: [0.1, 0.2, 0.3],
            pitch: 10,
            yaw: 20,
            head_yaw: 30,
            data: 0,
        })
        .encode();
        let out = translator.translate(&body, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x0F);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.u8().unwrap(), 54);
        assert_eq!(r.i32().unwrap(), 48); // 1.5 * 32
        assert_eq!(r.i32().unwrap(), 2048);
        assert_eq!(r.i32().unwrap(), -64);
        assert_eq!(r.i8().unwrap(), 20);
        assert_eq!(r.i8().unwrap(), 10);
        assert_eq!(r.i8().unwrap(), 30);
        assert_eq!(r.i16().unwrap(), 800);
        assert_eq!(r.i16().unwrap(), 1600);
        assert_eq!(r.i16().unwrap(), 2400);
        assert_eq!(r.u8().unwrap(), 0x7F);
    }

    #[test]
    fn moves_and_removes_entities_as_1_8_does() {
        let mut translator = PlayTranslator::default();
        let update = Clientbound::SetEntityMotion(SetEntityMotion {
            id: 7,
            velocity: [0.1, -0.2, 0.0],
        })
        .encode();
        let out = translator.translate(&update, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x12);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.i16().unwrap(), 800);
        assert_eq!(r.i16().unwrap(), -1600);
        assert_eq!(r.i16().unwrap(), 0);

        let teleport = Clientbound::TeleportEntity(TeleportEntity {
            id: 7,
            position: [1.0, 64.0, -2.0],
            delta: [0.0; 3],
            yaw: 90.0,
            pitch: -45.0,
            relatives: 0,
            on_ground: true,
        })
        .encode();
        let out = translator.translate(&teleport, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x18);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.i32().unwrap(), 32);
        assert_eq!(r.i32().unwrap(), 2048);
        assert_eq!(r.i32().unwrap(), -64);
        assert_eq!(r.i8().unwrap(), 64); // 90 degrees
        assert_eq!(r.i8().unwrap(), -32); // -45 degrees
        assert!(r.bool().unwrap());

        let remove = Clientbound::RemoveEntities(RemoveEntities { ids: vec![7, 9] }).encode();
        let out = translator.translate(&remove, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x13);
        assert_eq!(r.varint().unwrap(), 2);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.varint().unwrap(), 9);
    }

    #[test]
    fn sends_1_8_item_stacks_and_windows() {
        use crate::item::{Component, Stack, component};
        let mut translator = PlayTranslator::default();
        translator.set_items(std::sync::Arc::new(|id| match id {
            1 => Some("minecraft:stone".to_string()),
            2 => Some("minecraft:diamond_sword".to_string()),
            _ => None,
        }));
        let slot = Clientbound::ContainerSetSlot(ContainerSetSlot {
            container: 0,
            state: 0,
            slot: 36,
            item: Some(Stack::new(1, 64)),
        })
        .encode();
        let out = translator.translate(&slot, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x2F);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 36);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 64);
        assert_eq!(r.i16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0); // no NBT

        let mut sword = Stack::new(2, 1);
        sword.added.push((component::DAMAGE, Component::Int(7)));
        let content = Clientbound::ContainerSetContent(ContainerSetContent {
            container: 0,
            state: 0,
            items: vec![Some(Stack::new(1, 64)), None],
            carried: Some(sword),
        })
        .encode();
        let out = translator.translate(&content, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x30);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 2);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 64);
        assert_eq!(r.i16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), -1); // empty slot
        assert_eq!(r.i16().unwrap(), 276); // diamond sword
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 0); // durability stays out of metadata
        assert_eq!(r.u8().unwrap(), 0x0a); // named compound, empty name
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0x03); // TAG_Int Damage
        assert_eq!(r.u16().unwrap(), 6);
        assert_eq!(r.bytes(6).unwrap(), b"Damage");
        assert_eq!(r.i32().unwrap(), 7);
        assert_eq!(r.u8().unwrap(), 0);

        let data = Clientbound::ContainerSetData(ContainerSetData {
            container: 0,
            id: 1,
            value: 100,
        })
        .encode();
        let out = translator.translate(&data, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x31);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 100);

        let close = Clientbound::ContainerClose(ContainerClose { container: 0 }).encode();
        let out = translator.translate(&close, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x2E);
        assert_eq!(r.i8().unwrap(), 0);

        // Inventory index 0 is hotbar slot 36 in 1.8's player window.
        let inventory = Clientbound::SetPlayerInventory(SetPlayerInventory {
            slot: 0,
            item: Some(Stack::new(1, 1)),
        })
        .encode();
        let out = translator.translate(&inventory, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x2F);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 36);
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);

        // Inventory armor slots 36..39 become 1.8 slots 8..5.
        let helmet = Clientbound::SetPlayerInventory(SetPlayerInventory {
            slot: 39,
            item: Some(Stack::new(2, 1)),
        })
        .encode();
        let out = translator.translate(&helmet, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x2F);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 5);
        // The offhand has no 1.8 slot.
        let offhand = Clientbound::SetPlayerInventory(SetPlayerInventory {
            slot: 40,
            item: Some(Stack::new(1, 1)),
        })
        .encode();
        assert!(translator.translate(&offhand, stone).is_none());

        let equipment = Clientbound::SetEquipment(SetEquipment {
            id: 7,
            slots: vec![
                (0, Some(Stack::new(1, 1))),
                (4, Some(Stack::new(2, 1))),
                (1, Some(Stack::new(1, 1))), // offhand: dropped
            ],
        })
        .encode();
        let out = translator.translate(&equipment, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x04);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.i16().unwrap(), 0); // held
        assert_eq!(r.i16().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), 3); // chestplate
        assert_eq!(r.i16().unwrap(), 276);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.i16().unwrap(), -1);
    }

    #[test]
    fn relative_entity_movement_uses_1_8_units() {
        let mut translator = PlayTranslator::default();
        let update = Clientbound::MoveEntityPosRot(MoveEntityPosRot(MoveEntity {
            id: 7,
            delta: Some(VecDelta::Linear([1280, -256, 0])),
            rotation: Some((20, 10)),
            on_ground: true,
        }))
        .encode();
        let out = translator.translate(&update, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x17);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.i8().unwrap(), 10); // 1280 / 4096 -> 10 / 32
        assert_eq!(r.i8().unwrap(), -2);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.i8().unwrap(), 20);
        assert_eq!(r.i8().unwrap(), 10);
        assert!(r.bool().unwrap());

        let head = Clientbound::RotateHead(RotateHead {
            id: 7,
            head_yaw: 30,
        })
        .encode();
        let out = translator.translate(&head, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x19);
        assert_eq!(r.varint().unwrap(), 7);
        assert_eq!(r.i8().unwrap(), 30);
    }

    #[test]
    fn spawns_an_item_as_a_1_8_object() {
        let mut translator = PlayTranslator::default();
        translator.set_entities(std::sync::Arc::new(|id| {
            (id == 2).then(|| "minecraft:item".to_string())
        }));
        let body = Clientbound::AddEntity(AddEntity {
            id: 9,
            uuid: 0,
            kind: 2,
            position: [0.0, 64.0, 0.0],
            velocity: [0.0; 3],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 0,
        })
        .encode();
        let out = translator.translate(&body, stone).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 0x0E);
        assert_eq!(r.varint().unwrap(), 9);
        assert_eq!(r.u8().unwrap(), 2);
        assert_eq!(r.i32().unwrap(), 0);
        assert_eq!(r.i32().unwrap(), 2048);
        assert_eq!(r.i32().unwrap(), 0);
        assert_eq!(r.i32().unwrap(), 0); // object data
        assert_eq!(r.i16().unwrap(), 0);
    }

    fn spawn(dimension: &str, game_type: i32) -> SpawnInfo {
        SpawnInfo {
            dimension_type: 0,
            dimension: dimension.into(),
            seed: 0,
            game_type,
            previous_game_type: None,
            debug: false,
            flat: false,
            last_death: None,
            portal_cooldown: 0,
            sea_level: 63,
        }
    }

    #[test]
    fn translates_the_world_entry_and_status_packets() {
        let mut translator = PlayTranslator::default();

        let join = Clientbound::Login(Login {
            player_id: 42,
            hardcore: false,
            levels: vec!["minecraft:overworld".into()],
            max_players: 20,
            chunk_radius: 10,
            simulation_distance: 10,
            reduced_debug_info: false,
            show_death_screen: true,
            limited_crafting: false,
            spawn: spawn("minecraft:overworld", 1),
            online_mode: false,
            enforces_secure_chat: false,
        });
        let body = translator.translate(&join.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x01);
        assert_eq!(r.i32().unwrap(), 42);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.i8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 2);
        assert_eq!(r.u8().unwrap(), 20);
        assert_eq!(
            crate::translate::legacy::packets::read_string(&mut r).unwrap(),
            "default"
        );
        assert!(!r.bool().unwrap());

        let nether = Clientbound::Respawn(Respawn {
            spawn: spawn("minecraft:the_nether", 1),
            keep: 0,
        });
        let body = translator.translate(&nether.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x07);
        assert_eq!(r.i32().unwrap(), -1);
        assert_eq!(r.u8().unwrap(), 2);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(
            crate::translate::legacy::packets::read_string(&mut r).unwrap(),
            "default"
        );

        let alive = Clientbound::KeepAlive(KeepAlive { id: 77 });
        assert_eq!(
            translator.translate(&alive.encode(), stone).unwrap(),
            [0x00, 77]
        );

        let health = Clientbound::SetHealth(SetHealth {
            health: 20.0,
            food: 18,
            saturation: 5.0,
        });
        let body = translator.translate(&health.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x06);
        assert_eq!(r.f32().unwrap(), 20.0);
        assert_eq!(r.varint().unwrap(), 18);
        assert_eq!(r.f32().unwrap(), 5.0);

        let time = Clientbound::SetTime(SetTime {
            game_time: 1000,
            clocks: vec![ClockState {
                clock: 0,
                total_ticks: 6000,
                partial_tick: 0.0,
                rate: 1.0,
            }],
        });
        let body = translator.translate(&time.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x03);
        assert_eq!((r.i64().unwrap(), r.i64().unwrap()), (1000, 6000));

        let abilities = Clientbound::PlayerAbilities(PlayerAbilities {
            flags: 0x0F,
            flying_speed: 0.05,
            walking_speed: 0.1,
        });
        let body = translator.translate(&abilities.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x39);
        assert_eq!(r.i8().unwrap(), 0x0F);
        assert_eq!((r.f32().unwrap(), r.f32().unwrap()), (0.05, 0.1));

        let held = Clientbound::SetHeldSlot(SetHeldSlot { slot: 3 });
        let body = translator.translate(&held.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x09);
        assert_eq!(r.i8().unwrap(), 3);
    }

    #[test]
    fn translates_chat_blocks_and_sections() {
        let mut translator = PlayTranslator::default();
        let packet = Clientbound::SystemChat(play::SystemChat {
            content: Tag::Compound(
                [("text".into(), Tag::String("hi".into()))]
                    .into_iter()
                    .collect(),
            ),
            overlay: false,
        });
        let body = translator.translate(&packet.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x02);
        assert_eq!(
            crate::translate::legacy::packets::read_string(&mut r).unwrap(),
            "{\"text\":\"hi\"}"
        );
        assert_eq!(r.u8().unwrap(), 1);

        let change = Clientbound::BlockUpdate(BlockUpdate {
            pos: [1, 64, -2],
            state: 1,
        });
        let body = translator.translate(&change.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x23);
        assert_eq!(read_position(&mut r).unwrap(), (1, 64, -2));
        assert_eq!(r.varint().unwrap(), 0x10);

        // state 1 at local (2, 3, 4).
        let change = (1i64 << 12) | (2 << 8) | (4 << 4) | 3;
        let update = Clientbound::SectionBlocksUpdate(SectionBlocksUpdate {
            section: [3, 4, -2],
            changes: vec![change],
        });
        let body = translator.translate(&update.encode(), stone).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.varint().unwrap(), 0x22);
        assert_eq!((r.i32().unwrap(), r.i32().unwrap()), (3, -2));
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), (2 << 12) | (4 << 8) | 3);
        assert_eq!(r.u16().unwrap(), 0x10);
    }

    #[test]
    fn translates_the_guests_movement_keep_alive_and_slot() {
        let guest_movement =
            |id: i32, position: Option<(f64, f64, f64)>, look: Option<(f32, f32)>, ground: bool| {
                let mut w = crate::buf::Writer::new();
                w.varint(id);
                if let Some((x, y, z)) = position {
                    w.f64(x);
                    w.f64(y);
                    w.f64(z);
                }
                if let Some((yaw, pitch)) = look {
                    w.f32(yaw);
                    w.f32(pitch);
                }
                w.bool(ground);
                w.data
            };
        let modern = |body: &[u8]| {
            let mut r = Reader::new(body);
            let id = r.varint().unwrap();
            Serverbound::decode(id, r.rest()).unwrap()
        };
        let mut translator = GuestTranslator;

        let body = guest_movement(
            super::super::ids::play::serverbound::PLAYER_POSITION_AND_LOOK,
            Some((1.0, 64.0, -2.5)),
            Some((90.0, -10.0)),
            true,
        );
        match modern(&translator.from_guest(&body).unwrap()) {
            Serverbound::MovePlayerPosRot(m) => {
                assert_eq!((m.x, m.y, m.z), (1.0, 64.0, -2.5));
                assert_eq!((m.yaw, m.pitch), (90.0, -10.0));
                assert_eq!(m.flags, 1);
            }
            other => panic!("{other:?}"),
        }
        let body = guest_movement(
            super::super::ids::play::serverbound::PLAYER,
            None,
            None,
            false,
        );
        match modern(&translator.from_guest(&body).unwrap()) {
            Serverbound::MovePlayerStatus(m) => assert_eq!(m.flags, 0),
            other => panic!("{other:?}"),
        }
        let body = guest_movement(
            super::super::ids::play::serverbound::PLAYER_POSITION,
            Some((1.0, 2.0, 3.0)),
            None,
            true,
        );
        match modern(&translator.from_guest(&body).unwrap()) {
            Serverbound::MovePlayerPos(m) => {
                assert_eq!((m.x, m.y, m.z, m.flags), (1.0, 2.0, 3.0, 1))
            }
            other => panic!("{other:?}"),
        }

        let mut keep = crate::buf::Writer::new();
        keep.varint(super::super::ids::play::serverbound::KEEP_ALIVE);
        keep.varint(42);
        match modern(&translator.from_guest(&keep.data).unwrap()) {
            Serverbound::KeepAlive(k) => assert_eq!(k.id, 42),
            other => panic!("{other:?}"),
        }

        let mut slot = crate::buf::Writer::new();
        slot.varint(super::super::ids::play::serverbound::HELD_ITEM_CHANGE);
        slot.u16(3);
        match modern(&translator.from_guest(&slot.data).unwrap()) {
            Serverbound::SetCarriedItem(s) => assert_eq!(s.slot, 3),
            other => panic!("{other:?}"),
        }

        let mut chat = crate::buf::Writer::new();
        chat.varint(super::super::ids::play::serverbound::CHAT);
        assert!(translator.from_guest(&chat.data).is_none());
    }

    #[test]
    fn packets_without_a_1_8_equivalent_are_dropped() {
        let mut translator = PlayTranslator::default();
        let packet = Clientbound::SetExperience(SetExperience {
            progress: 0.5,
            level: 3,
            total: 100,
        });
        assert!(translator.translate(&packet.encode(), stone).is_none());
        let high = Clientbound::BlockUpdate(BlockUpdate {
            pos: [0, 300, 0],
            state: 1,
        });
        assert!(translator.translate(&high.encode(), stone).is_none());
        let high = Clientbound::SectionBlocksUpdate(SectionBlocksUpdate {
            section: [0, 20, 0],
            changes: Vec::new(),
        });
        assert!(translator.translate(&high.encode(), stone).is_none());
        let low = Clientbound::BlockUpdate(BlockUpdate {
            pos: [0, -1, 0],
            state: 1,
        });
        assert!(translator.translate(&low.encode(), stone).is_none());
    }

    #[test]
    fn translates_a_modern_chunk_to_1_8() {
        use crate::chunk::{ChunkData, LightData, Paletted, Section, Strategy};
        let biomes = Paletted::from_values(&[1; 64], Strategy::Biomes, 7);
        let air = Section {
            non_empty: 0,
            fluids: 0,
            states: Paletted::from_values(&[0; 4096], Strategy::Blocks, 16),
            biomes: biomes.clone(),
        };
        let stone_section = Section {
            non_empty: 4096,
            fluids: 0,
            states: Paletted::from_values(&[1; 4096], Strategy::Blocks, 16),
            biomes,
        };
        let mut parts = vec![air; 16];
        parts[0] = stone_section;
        let chunk = ChunkData {
            heightmaps: Vec::new(),
            buffer: ChunkData::from_sections(&parts),
            block_entities: Vec::new(),
        };
        let light = LightData {
            sky_mask: Vec::new(),
            block_mask: Vec::new(),
            empty_sky_mask: Vec::new(),
            empty_block_mask: Vec::new(),
            sky: Vec::new(),
            block: Vec::new(),
        };
        let shape = ChunkShape {
            sections: 16,
            first_1_8: 0,
            block_bits: 16,
            biome_bits: 7,
            min_y: 0,
            sky_light: false,
        };
        let body = chunk_data(3, -4, &chunk, &light, shape, stone, |_| 1).unwrap();
        let mut r = Reader::new(&body);
        assert_eq!(r.i32().unwrap(), 3);
        assert_eq!(r.i32().unwrap(), -4);
        assert!(r.bool().unwrap());
        assert_eq!(r.u16().unwrap(), 0x0001);
        assert_eq!(r.varint().unwrap(), 8192 + 2048 + 256);
        // The stone section, its dark block light, and plains biomes.
        assert_eq!(&body[13..15], &[0x00, 0x10]);
        assert_eq!(&body[13 + 8192..13 + 8194], &[0, 0]);
        assert_eq!(&body[13 + 8192 + 2048..13 + 8192 + 2050], &[1, 1]);
        // No chunks without a shape.
        let packet = Clientbound::LevelChunkWithLight(crate::packets::play::LevelChunkWithLight {
            x: 3,
            z: -4,
            chunk,
            light: LightData {
                sky_mask: Vec::new(),
                block_mask: Vec::new(),
                empty_sky_mask: Vec::new(),
                empty_block_mask: Vec::new(),
                sky: Vec::new(),
                block: Vec::new(),
            },
        });
        let mut translator = PlayTranslator::default();
        assert!(translator.translate(&packet.encode(), stone).is_none());
    }
}
