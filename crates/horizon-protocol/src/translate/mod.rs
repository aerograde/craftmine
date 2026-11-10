//! Translating the world's game protocol for guests that speak another
//! Minecraft version (D3/D4 of the shared-world plan). Each version gets a
//! codec that turns the world's packets into its packets and back; the
//! per-version encodings live beside this module.
//!
//! [`legacy`] is protocol 47 (Minecraft 1.8): block states and chunks, the
//! login and play packets the world needs, and the guest's movement. Later
//! versions add a sibling module and join `EaglerServerSocket`'s translation.
pub mod legacy;
pub mod mappings_26_2;
pub mod modern;
pub mod recipe_display;
