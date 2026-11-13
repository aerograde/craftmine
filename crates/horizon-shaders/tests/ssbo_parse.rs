use naga::ShaderStage;
use naga::front::glsl::{Frontend, Options};

#[test]
fn a_buffer_block_parses() {
    let source = r#"#version 450
layout(std430, set = 4, binding = 0) buffer environmentData {
    vec3 sunlightColor;
    vec3 skylightColor;
    float weatherFrameTimeCounter;
    uint encodedHeldLightColor;
};
void main() {
    sunlightColor = vec3(1.0);
}
"#;
    let mut frontend = Frontend::default();
    let result = frontend.parse(&Options::from(ShaderStage::Vertex), source);
    assert!(
        result.is_ok(),
        "{}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
}
