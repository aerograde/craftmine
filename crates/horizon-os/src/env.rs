//! The process's folders and arguments. The browser has neither a working
//! directory nor an executable: the virtual filesystem's root stands in for
//! both, and the page hands the arguments to `horizon_start`.

#[cfg(not(target_arch = "wasm32"))]
pub use std::env::{args, current_dir, current_exe, temp_dir};

/// One of the page's `horizonEnv` variables, or a `?env=NAME=VALUE` of the
/// URL: the environment the native build reads, which the browser has none of.
/// `None` when unset, as std's `var_os`.
#[cfg(target_arch = "wasm32")]
pub fn var(name: &str) -> std::option::Option<std::string::String> {
    crate::web::env(name)
}

/// The environment's flags on the native build.
#[cfg(not(target_arch = "wasm32"))]
pub fn var(name: &str) -> std::option::Option<std::string::String> {
    std::env::var(name).ok()
}

/// Whether the environment has `name` at all (its value unused).
pub fn has(name: &str) -> bool {
    var(name).is_some()
}

/// The arguments the page started the game with, after a program name as
/// std's `args` has.
#[cfg(target_arch = "wasm32")]
pub fn args() -> std::vec::IntoIter<String> {
    let mut args = vec!["horizon-client".to_string()];
    args.extend(crate::web::args());
    args.into_iter()
}

/// The game's folder: the virtual filesystem's root.
#[cfg(target_arch = "wasm32")]
pub fn current_dir() -> std::io::Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from("/"))
}

/// A program file in the root, so its parent is the game's folder.
#[cfg(target_arch = "wasm32")]
pub fn current_exe() -> std::io::Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from("/horizon-client"))
}

/// Scratch files: a folder of the virtual filesystem.
#[cfg(target_arch = "wasm32")]
pub fn temp_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp")
}
