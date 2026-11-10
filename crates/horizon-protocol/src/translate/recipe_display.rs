//! The recipe book's displays as a 26.2 guest reads them.
//!
//! Both releases register `minecraft:slot_display` with the same names in the
//! same order, and `minecraft:recipe_display` likewise, so no type id moves.
//! What did change is the body of two slot displays, and every slot display
//! that names an item or a data component names a 26.3 registry id:
//!
//! - `tag` carries a `HolderSet<Item>` in 26.3 but only a tag key in 26.2. A
//!   tag key passes through; a direct item list becomes a `composite` of
//!   `item` displays, which is how ViaBackwards keeps the option list.
//! - `only_with_component` names a 26.3 data component type, and 26.2's ids
//!   are not its own.
//!
//! Item ids are remapped through [`map::ITEMS`] and component ids through
//! [`map::COMPONENTS`]. A 26.2 client resolves both in its own registries, so
//! a 26.3 id that 26.2 has no id for must not be forwarded: an item becomes
//! an `empty` display, a component becomes 0 (the default, as ViaBackwards
//! maps it) and an ingredient [`HolderSet`] drops the id. 26.3 component
//! payloads inside an `item_stack` display are dropped, as the item packets in
//! [`super::modern`] already drop them.
//!
//! The two 26.2/26.3 differences above come from the reference rewriters
//! (`RecipeDisplayRewriter`'s `tag`/`only_with_component` handlers and
//! ViaBackwards' `Protocol26_3To26_2` slot-display override); the registry
//! orders were checked against both releases' `registries.json` reports.
use crate::{
    buf::{Codec, Reader, Writer},
    item::Template,
    packets::recipes::HolderSet,
    translate::mappings_26_2 as map,
};

/// `minecraft:slot_display` ids, the same in 26.2 and 26.3.
const EMPTY: i32 = 0;
const ANY_FUEL: i32 = 1;
const WITH_ANY_POTION: i32 = 2;
const ONLY_WITH_COMPONENT: i32 = 3;
const ITEM: i32 = 4;
const ITEM_STACK: i32 = 5;
const TAG: i32 = 6;
const DYED: i32 = 7;
const SMITHING_TRIM: i32 = 8;
const WITH_REMAINDER: i32 = 9;
const COMPOSITE: i32 = 10;

/// How deep slot displays may nest, as [`crate::packets::recipes`] bounds
/// them.
const MAX_DEPTH: u32 = 32;

/// `ClientboundRecipeBookAddPacket`, the entries and the replace flag.
pub fn book_add(rest: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let mut w = Writer::new();
    let entries = r.count(r.remaining()).ok()?;
    w.varint(entries as i32);
    for _ in 0..entries {
        w.varint(r.varint().ok()?); // display id
        recipe_display(&mut r, &mut w)?;
        w.varint(r.varint().ok()?); // group
        w.varint(r.varint().ok()?); // category
        if r.bool().ok()? {
            let ingredients = r.count(r.remaining()).ok()?;
            w.bool(true);
            w.varint(ingredients as i32);
            for _ in 0..ingredients {
                ingredient(&mut r, &mut w)?;
            }
        } else {
            w.bool(false);
        }
        w.i8(r.i8().ok()?); // flags
    }
    w.bool(r.bool().ok()?); // replace
    finished(&r, w)
}

/// `ClientboundPlaceGhostRecipePacket`: the container, then the display.
pub fn place_ghost_recipe(rest: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let mut w = Writer::new();
    w.varint(r.varint().ok()?); // container id
    recipe_display(&mut r, &mut w)?;
    finished(&r, w)
}

/// `ClientboundUpdateRecipesPacket`: the recipe property sets and the
/// stonecutter's selectable recipes.
pub fn update_recipes(rest: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let mut w = Writer::new();
    let sets = r.count(r.remaining()).ok()?;
    w.varint(sets as i32);
    for _ in 0..sets {
        let name = r.string(32767).ok()?;
        w.string(&name);
        let ids = r.count(r.remaining()).ok()?;
        let mut mapped = Vec::with_capacity(ids.min(1024));
        for _ in 0..ids {
            let id = r.varint().ok()?;
            // `RecipePropertySet` holds item holders, looked up by id.
            if let Some(id) = map::to_26_2(&map::ITEMS, id) {
                mapped.push(id);
            }
        }
        w.varint(mapped.len() as i32);
        mapped.iter().for_each(|id| w.varint(*id));
    }
    let stonecutting = r.count(r.remaining()).ok()?;
    w.varint(stonecutting as i32);
    for _ in 0..stonecutting {
        ingredient(&mut r, &mut w)?; // the input
        slot_display(&mut r, &mut w, 0)?; // what it makes
    }
    finished(&r, w)
}

/// The body is exactly the shape this rewrite knows: anything else is a
/// version difference not handled here, and a guest sent it would misread the
/// rest of the packet, so the caller drops it instead.
fn finished(r: &Reader, w: Writer) -> Option<Vec<u8>> {
    (r.remaining() == 0).then_some(w.data)
}

/// `RecipeDisplay`: the type, then its slot displays (and a furnace's timing).
fn recipe_display(r: &mut Reader, w: &mut Writer) -> Option<()> {
    let kind = r.varint().ok()?;
    w.varint(kind);
    match kind {
        // Shapeless: ingredients, result, station.
        0 => {
            slot_displays(r, w)?;
            slot_display(r, w, 0)?;
            slot_display(r, w, 0)?;
        }
        // Shaped: width, height, ingredients, result, station.
        1 => {
            w.varint(r.varint().ok()?);
            w.varint(r.varint().ok()?);
            slot_displays(r, w)?;
            slot_display(r, w, 0)?;
            slot_display(r, w, 0)?;
        }
        // Furnace: ingredient, fuel, result, station, duration, experience.
        2 => {
            for _ in 0..4 {
                slot_display(r, w, 0)?;
            }
            w.varint(r.varint().ok()?);
            w.f32(r.f32().ok()?);
        }
        // Stonecutter: input, result, station.
        3 => {
            for _ in 0..3 {
                slot_display(r, w, 0)?;
            }
        }
        // Smithing: template, base, addition, result, station.
        4 => {
            for _ in 0..5 {
                slot_display(r, w, 0)?;
            }
        }
        _ => return None,
    }
    Some(())
}

fn slot_displays(r: &mut Reader, w: &mut Writer) -> Option<()> {
    let n = r.count(r.remaining()).ok()?;
    w.varint(n as i32);
    for _ in 0..n {
        slot_display(r, w, 1)?;
    }
    Some(())
}

/// One `SlotDisplay`, rewritten into 26.2's bytes.
fn slot_display(r: &mut Reader, w: &mut Writer, depth: u32) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    let kind = r.varint().ok()?;
    match kind {
        EMPTY | ANY_FUEL => w.varint(kind),
        WITH_ANY_POTION => {
            w.varint(kind);
            slot_display(r, w, depth + 1)?;
        }
        ONLY_WITH_COMPONENT => {
            w.varint(kind);
            slot_display(r, w, depth + 1)?;
            let kind = r.varint().ok()?;
            // 26.2 has no id for a component 26.3 introduced; 0 is the
            // default its own rewriter maps to.
            w.varint(map::to_26_2(&map::COMPONENTS, kind).unwrap_or(0));
        }
        ITEM => {
            let id = r.varint().ok()?;
            write_item(id, w);
        }
        ITEM_STACK => {
            let mut stack = Template::read(r).ok()?;
            let Some(item) = map::to_26_2(&map::ITEMS, stack.item) else {
                // A 26.3 id 26.2 has no item for: show an empty slot. At
                // depth, `empty` is a display like any other.
                return write_display(EMPTY, w);
            };
            stack.item = item;
            // 26.3's component ids and payloads are not 26.2's.
            stack.added.clear();
            stack.removed.clear();
            w.varint(ITEM_STACK);
            Template::write(&stack, w);
        }
        TAG => match HolderSet::read(r).ok()? {
            // 26.2's tag display takes only a tag key.
            HolderSet::Tag(name) => {
                w.varint(TAG);
                w.string(&name);
            }
            // A direct item list has no 26.2 tag display form, so it becomes
            // the equivalent composite of item displays.
            HolderSet::Items(ids) => {
                w.varint(COMPOSITE);
                w.varint(ids.len() as i32);
                ids.iter().for_each(|id| write_item(*id, w));
            }
        },
        DYED | WITH_REMAINDER => {
            w.varint(kind);
            slot_display(r, w, depth + 1)?;
            slot_display(r, w, depth + 1)?;
        }
        SMITHING_TRIM => {
            w.varint(kind);
            slot_display(r, w, depth + 1)?; // base
            slot_display(r, w, depth + 1)?; // material
            // The trim pattern is `ByteBufCodecs.holder` in both releases: a
            // registry id (0 is a direct pattern), whose direct form's
            // description is a chat component and is left alone.
            let pattern = r.varint().ok()?;
            w.varint(pattern);
            if pattern == 0 {
                let asset = r.string(32767).ok()?;
                w.string(&asset);
                let description = r.nbt().ok()?;
                w.nbt(&description);
                w.bool(r.bool().ok()?);
            }
        }
        COMPOSITE => {
            w.varint(kind);
            let n = r.count(r.remaining()).ok()?;
            w.varint(n as i32);
            for _ in 0..n {
                slot_display(r, w, depth + 1)?;
            }
        }
        _ => return None,
    }
    Some(())
}

/// An `item` slot display for a 26.3 item id, or `empty` when 26.2 has no id
/// for it.
fn write_item(id: i32, w: &mut Writer) {
    match map::to_26_2(&map::ITEMS, id) {
        Some(id) => {
            w.varint(ITEM);
            w.varint(id);
        }
        None => {
            w.varint(EMPTY);
        }
    }
}

fn write_display(kind: i32, w: &mut Writer) -> Option<()> {
    w.varint(kind);
    Some(())
}

/// `HolderSet<Item>` in 26.2: a tag key, or the item ids 26.2 has.
fn ingredient(r: &mut Reader, w: &mut Writer) -> Option<()> {
    match HolderSet::read(r).ok()? {
        HolderSet::Tag(name) => {
            w.varint(0);
            w.string(&name);
        }
        HolderSet::Items(ids) => {
            let mut mapped = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(id) = map::to_26_2(&map::ITEMS, id) {
                    mapped.push(id);
                }
            }
            w.varint(mapped.len() as i32 + 1);
            mapped.iter().for_each(|id| w.varint(*id));
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        item::{Component, Stack},
        packets::recipes::{RecipeBookAdd, RecipeBookEntry, RecipeDisplay, SlotDisplay},
        translate::modern::rewrite_clientbound_play,
    };

    /// 26.3 item 73 is 26.2 item 72 (ViaVersion's shift table).
    const HOST_ITEM: i32 = 73;
    const GUEST_ITEM: i32 = 72;

    fn book_add_body(display: RecipeDisplay, requirements: Option<Vec<HolderSet>>) -> Vec<u8> {
        let add = RecipeBookAdd {
            entries: vec![RecipeBookEntry {
                id: 7,
                display,
                group: 3,
                category: 4,
                requirements,
                flags: 2,
            }],
            replace: true,
        };
        let mut w = Writer::new();
        <RecipeBookAdd as Codec<RecipeBookAdd>>::write(&add, &mut w);
        w.data
    }

    #[test]
    fn a_tag_slot_display_keeps_a_tag_and_turns_item_lists_into_a_composite() {
        let display = RecipeDisplay::Shapeless {
            ingredients: vec![
                SlotDisplay::Tag(HolderSet::Tag("minecraft:planks".into())),
                SlotDisplay::Tag(HolderSet::Items(vec![HOST_ITEM])),
            ],
            result: SlotDisplay::Item(HOST_ITEM),
            station: SlotDisplay::Item(HOST_ITEM),
        };
        let mut body = Writer::new();
        body.varint(crate::ids::play::clientbound::RECIPE_BOOK_ADD);
        body.bytes(&book_add_body(
            display,
            Some(vec![HolderSet::Items(vec![HOST_ITEM])]),
        ));

        let out = rewrite_clientbound_play(&body.data).unwrap();
        let (id, rest) = super::super::modern::split_for_tests(&out).unwrap();
        assert_eq!(id, 74, "recipe_book_add moves down one place");
        let mut r = Reader::new(rest);
        assert_eq!(r.varint().unwrap(), 1); // one entry
        assert_eq!(r.varint().unwrap(), 7); // display id

        // 26.2's `tag` is a bare string, not a holder set: read the guest's
        // wire form directly.
        assert_eq!(r.varint().unwrap(), 0, "shapeless");
        assert_eq!(r.varint().unwrap(), 2, "two ingredients");
        assert_eq!(r.varint().unwrap(), TAG);
        assert_eq!(r.string(32767).unwrap(), "minecraft:planks");
        assert_eq!(r.varint().unwrap(), COMPOSITE);
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), GUEST_ITEM);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), GUEST_ITEM);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), GUEST_ITEM);
        assert_eq!(r.varint().unwrap(), 3); // group
        assert_eq!(r.varint().unwrap(), 4); // category
        assert!(r.bool().unwrap()); // requirements follow
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(
            HolderSet::read(&mut r).unwrap(),
            HolderSet::Items(vec![GUEST_ITEM])
        );
        assert_eq!(r.i8().unwrap(), 2); // flags
        assert!(r.bool().unwrap()); // replace
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn a_shaped_display_remaps_its_component_type_and_keeps_its_size() {
        let display = RecipeDisplay::Shaped {
            width: 2,
            height: 1,
            ingredients: vec![
                SlotDisplay::OnlyWithComponent(Box::new(SlotDisplay::Item(HOST_ITEM)), 42),
                SlotDisplay::OnlyWithComponent(Box::new(SlotDisplay::AnyFuel), 41),
            ],
            result: SlotDisplay::Item(HOST_ITEM),
            station: SlotDisplay::Empty,
        };
        let out = book_add(&book_add_body(display, None)).unwrap();
        let mut r = Reader::new(&out);
        r.varint().unwrap(); // entry count
        r.varint().unwrap(); // display id
        assert_eq!(r.varint().unwrap(), 1, "shaped");
        assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (2, 1));
        assert_eq!(r.varint().unwrap(), 2, "two ingredients");
        assert_eq!(r.varint().unwrap(), ONLY_WITH_COMPONENT);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), GUEST_ITEM);
        // 26.3 component 42 is 26.2's 41; 26.3-only 41 falls back to 0.
        assert_eq!(r.varint().unwrap(), 41);
        assert_eq!(r.varint().unwrap(), ONLY_WITH_COMPONENT);
        assert_eq!(r.varint().unwrap(), ANY_FUEL);
        assert_eq!(r.varint().unwrap(), 0);
    }

    #[test]
    fn an_item_stack_display_drops_26_3_components() {
        let mut stack = Stack::new(HOST_ITEM, 4);
        stack.added.push((3, Component::Int(9)));
        stack.removed.push(7);
        let display = RecipeDisplay::Stonecutter {
            input: SlotDisplay::Item(HOST_ITEM),
            result: SlotDisplay::ItemStack(stack),
            station: SlotDisplay::Item(HOST_ITEM),
        };
        let out = place_ghost_recipe(&{
            let mut w = Writer::new();
            w.varint(9); // container
            RecipeDisplay::write(&display, &mut w);
            w.data
        })
        .unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 9);
        assert_eq!(r.varint().unwrap(), 3, "stonecutter");
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), GUEST_ITEM);
        assert_eq!(r.varint().unwrap(), ITEM_STACK);
        let stack = Template::read(&mut r).unwrap();
        assert_eq!(stack.item, GUEST_ITEM);
        assert_eq!(stack.count, 4);
        assert!(stack.added.is_empty(), "26.3 components are dropped");
        assert!(stack.removed.is_empty());
    }

    #[test]
    fn a_direct_trim_pattern_passes_through_with_its_description() {
        let pattern = TrimPatternDirect {
            asset: "minecraft:trim/quartz".into(),
            description: horizon_world::nbt::Tag::String("Quartz".into()),
            decal: true,
        };
        let mut w = Writer::new();
        w.varint(8); // smithing_trim
        w.varint(ITEM); // base
        w.varint(HOST_ITEM);
        w.varint(DYED); // material: a dyed target
        w.varint(ITEM);
        w.varint(5);
        w.varint(ITEM);
        w.varint(6);
        w.varint(0); // a direct pattern
        w.string(&pattern.asset);
        w.nbt(&pattern.description);
        w.bool(pattern.decal);
        let mut input = Reader::new(&w.data);
        let mut out = Writer::new();
        slot_display(&mut input, &mut out, 0).unwrap();
        assert_eq!(input.remaining(), 0);
        let mut r = Reader::new(&out.data);
        assert_eq!(r.varint().unwrap(), SMITHING_TRIM);
        assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (ITEM, 72));
        assert_eq!(r.varint().unwrap(), DYED);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), 5);
        assert_eq!(r.varint().unwrap(), ITEM);
        assert_eq!(r.varint().unwrap(), 6);
        assert_eq!(r.varint().unwrap(), 0, "direct pattern");
        assert_eq!(r.string(32767).unwrap(), pattern.asset);
        assert_eq!(r.nbt().unwrap(), pattern.description);
        assert!(r.bool().unwrap());
    }

    #[test]
    fn update_recipes_remaps_the_property_sets_and_the_stonecutter() {
        let mut w = Writer::new();
        w.varint(1); // property set count
        w.string("minecraft:smithing_base");
        w.varint(2);
        w.varint(HOST_ITEM);
        w.varint(HOST_ITEM);
        w.varint(1); // stonecutter recipe count
        w.varint(2); // an ingredient of one id...
        w.varint(HOST_ITEM);
        w.varint(ITEM_STACK); // ...making an item_stack display
        w.varint(HOST_ITEM);
        w.varint(1);
        w.varint(0);
        w.varint(0);
        let out = update_recipes(&w.data).unwrap();
        let mut r = Reader::new(&out);
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(r.string(32767).unwrap(), "minecraft:smithing_base");
        assert_eq!(r.varint().unwrap(), 2);
        assert_eq!(
            (r.varint().unwrap(), r.varint().unwrap()),
            (GUEST_ITEM, GUEST_ITEM)
        );
        assert_eq!(r.varint().unwrap(), 1);
        assert_eq!(
            HolderSet::read(&mut r).unwrap(),
            HolderSet::Items(vec![GUEST_ITEM])
        );
        assert_eq!(r.varint().unwrap(), ITEM_STACK);
        let stack = Template::read(&mut r).unwrap();
        assert_eq!((stack.item, stack.count), (GUEST_ITEM, 1));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn an_unmapped_item_becomes_an_empty_display_and_leaves_the_stream_intact() {
        // 26.3's items all have a 26.2 id; only a nonsense id reaches this.
        let mut w = Writer::new();
        w.varint(ITEM);
        w.varint(9000);
        w.varint(ITEM);
        w.varint(HOST_ITEM);
        let out = place_ghost_recipe(&{
            let mut body = Writer::new();
            body.varint(1);
            body.varint(0); // composite, the display for this test
            body.varint(2);
            body.bytes(&w.data);
            body.data
        });
        assert!(out.is_none(), "the padded body is not a display's shape");
    }

    #[test]
    fn an_unknown_body_shape_is_dropped_rather_than_missent() {
        // A trailing byte means this is not the packet shape the rewrite
        // knows; forwarding it would desync the guest's stream.
        let mut w = Writer::new();
        w.varint(1);
        w.varint(7);
        w.varint(0); // shapeless
        w.varint(0); // no ingredients
        w.varint(EMPTY); // result
        w.varint(EMPTY); // station
        w.varint(0);
        w.varint(0);
        w.bool(false);
        w.i8(0);
        w.bool(true);
        w.data.push(0);
        assert!(book_add(&w.data).is_none());
    }

    struct TrimPatternDirect {
        asset: String,
        description: horizon_world::nbt::Tag,
        decal: bool,
    }
}
