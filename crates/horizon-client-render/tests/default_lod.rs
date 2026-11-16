//! A fresh install gets a bounded horizon.

#[test]
fn the_default_horizon_is_bounded_at_96_chunks() {
    let g = horizon_client_render::graphics::Graphics::default();
    assert_eq!(g.lod_distance, 96);
    assert!(!g.lod_infinite);
    assert_eq!(g.shader_pack, "MiniatureXL");
}
