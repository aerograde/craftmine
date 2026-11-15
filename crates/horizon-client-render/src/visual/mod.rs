//! Horizon's built-in visual profile: one Rust-owned set of parameters, pass
//! decisions and CPU reference math. The GPU kernels consume the same values.
//! The lighting and atmosphere model is a port of Minecraft Miniature Shader
//! (MIT); see `THIRD_PARTY_NOTICES.md`.

pub mod atmosphere;
pub mod grade;
pub mod graph;
pub mod miniature;
pub mod ref_math;
mod tier;

pub use graph::{PassSet, passes};
pub use tier::{Params, ShadowPlan, Tier};
