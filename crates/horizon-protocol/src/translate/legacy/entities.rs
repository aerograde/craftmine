//! The 1.8 spawn type for a modern entity name.
//!
//! Protocol 47 has two spawn packets with overlapping numeric spaces: a
//! living entity uses `Spawn Mob`'s type (48..120), an object uses
//! `Spawn Object`'s (1..90). The names and ids come from the canonical 1.8
//! entity list (PrismarineJS `minecraft-data`); modern names that were
//! renamed since 1.8 are mapped explicitly.

/// Which 1.8 spawn packet a modern entity type takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spawn {
    Mob(u8),
    Object(u8),
}

/// The 1.8 spawn type for a modern entity name (`minecraft:zombie` or
/// `zombie`), or `None` when 1.8 has no such entity.
pub fn spawn_kind(name: &str) -> Option<Spawn> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    Some(match name {
        // Living entities, 1.8 `Spawn Mob`.
        "mob" => Spawn::Mob(48),
        "monster" => Spawn::Mob(49),
        "creeper" => Spawn::Mob(50),
        "skeleton" | "stray" | "bogged" => Spawn::Mob(51),
        "spider" => Spawn::Mob(52),
        "giant" => Spawn::Mob(53),
        "zombie" | "husk" | "drowned" => Spawn::Mob(54),
        "slime" => Spawn::Mob(55),
        "ghast" => Spawn::Mob(56),
        "zombified_piglin" | "pig_zombie" => Spawn::Mob(57),
        "enderman" => Spawn::Mob(58),
        "cave_spider" => Spawn::Mob(59),
        "silverfish" => Spawn::Mob(60),
        "blaze" => Spawn::Mob(61),
        "magma_cube" | "lava_slime" => Spawn::Mob(62),
        "ender_dragon" => Spawn::Mob(63),
        "wither" | "wither_boss" => Spawn::Mob(64),
        "bat" => Spawn::Mob(65),
        "witch" => Spawn::Mob(66),
        "endermite" => Spawn::Mob(67),
        "guardian" | "elder_guardian" => Spawn::Mob(68),
        "pig" => Spawn::Mob(90),
        "sheep" => Spawn::Mob(91),
        "cow" => Spawn::Mob(92),
        "chicken" => Spawn::Mob(93),
        "squid" => Spawn::Mob(94),
        "wolf" => Spawn::Mob(95),
        "mooshroom" | "mushroom_cow" => Spawn::Mob(96),
        "snow_golem" | "snow_man" => Spawn::Mob(97),
        "ocelot" | "ozelot" | "cat" => Spawn::Mob(98),
        "iron_golem" | "villager_golem" => Spawn::Mob(99),
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => Spawn::Mob(100),
        "rabbit" => Spawn::Mob(101),
        "villager" | "wandering_trader" => Spawn::Mob(120),
        // Objects, 1.8 `Spawn Object`.
        "experience_orb" => Spawn::Object(1),
        "item" => Spawn::Object(2),
        "boat" | "oak_boat" | "spruce_boat" | "birch_boat" | "jungle_boat" | "acacia_boat"
        | "dark_oak_boat" => Spawn::Object(1),
        "minecart" | "chest_minecart" | "furnace_minecart" | "hopper_minecart" => Spawn::Object(10),
        "tnt" | "primed_tnt" => Spawn::Object(50),
        "arrow" => Spawn::Object(60),
        "snowball" => Spawn::Object(61),
        "egg" | "thrown_egg" => Spawn::Object(62),
        "fireball" => Spawn::Object(63),
        "small_fireball" => Spawn::Object(64),
        "ender_pearl" | "thrown_ender_pearl" => Spawn::Object(65),
        "wither_skull" => Spawn::Object(66),
        "falling_block" | "falling_sand" => Spawn::Object(70),
        "item_frame" => Spawn::Object(71),
        "eye_of_ender" => Spawn::Object(72),
        "potion" | "splash_potion" | "lingering_potion" => Spawn::Object(73),
        "experience_bottle" => Spawn::Object(75),
        "firework_rocket" => Spawn::Object(76),
        "leash_knot" => Spawn::Object(77),
        "armor_stand" => Spawn::Object(78),
        "fishing_bobber" => Spawn::Object(90),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_use_the_right_spawn_packet() {
        assert_eq!(spawn_kind("minecraft:zombie"), Some(Spawn::Mob(54)));
        assert_eq!(spawn_kind("creeper"), Some(Spawn::Mob(50)));
        assert_eq!(spawn_kind("minecraft:item"), Some(Spawn::Object(2)));
        assert_eq!(spawn_kind("minecraft:arrow"), Some(Spawn::Object(60)));
        // The same number means different things in the two packets.
        assert_eq!(spawn_kind("tnt"), Some(Spawn::Object(50)));
        assert_eq!(spawn_kind("creeper"), Some(Spawn::Mob(50)));
        // Renames since 1.8.
        assert_eq!(spawn_kind("zombified_piglin"), Some(Spawn::Mob(57)));
        assert_eq!(spawn_kind("mooshroom"), Some(Spawn::Mob(96)));
        assert_eq!(spawn_kind("snow_golem"), Some(Spawn::Mob(97)));
        assert_eq!(spawn_kind("iron_golem"), Some(Spawn::Mob(99)));
        assert_eq!(spawn_kind("minecraft:unknown"), None);
    }
}
