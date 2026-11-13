//! The page's side of the game: what the page calls before and to start the
//! game, and where the log and panics go (the browser's console).

use std::sync::Mutex;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function horizon_console(level, message) {
    if (level <= 1) console.error(message);
    else if (level === 2) console.warn(message);
    else console.log(message);
}

export function horizon_js_stack() {
    try { throw new Error("stack"); } catch (e) { return (e && e.stack) || ""; }
}

let wantLock = false;
let lostLock = false;
let hooked = null;
export function horizon_pointer_lock_lost() {
    const lost = lostLock;
    lostLock = false;
    return lost;
}
function lock(canvas) {
    // Refused outside a click or key press: by a rejected promise, or a throw.
    try {
        const p = canvas.requestPointerLock();
        if (p && p.catch) p.catch(() => {});
    } catch (e) {}
}
export function horizon_pointer_locked() {
    const canvas = document.querySelector("canvas");
    return !!canvas && document.pointerLockElement === canvas;
}
export function horizon_pointer_lock(want) {
    wantLock = want;
    const canvas = document.querySelector("canvas");
    if (!canvas) return;
    if (hooked === null) {
        // Escape while the pointer is locked only releases the lock: the page
        // never sees the key, so the game hears of it as a lost lock.
        document.addEventListener("pointerlockchange", () => {
            if (wantLock && !document.pointerLockElement) lostLock = true;
        });
    }
    if (hooked !== canvas) {
        hooked = canvas;
        // Browsers refuse the lock asked with Escape (no user gesture), and for
        // a moment after Escape let it go: the game then waits for the next
        // click or key. The click that takes the lock only takes the lock, so
        // it doesn't swing a sword.
        let swallowUp = false;
        window.addEventListener("pointerdown", (e) => {
            if (wantLock && e.target === canvas && document.pointerLockElement !== canvas) {
                lock(canvas);
                swallowUp = true;
                e.stopImmediatePropagation();
            }
        }, true);
        window.addEventListener("pointerup", (e) => {
            if (swallowUp) {
                swallowUp = false;
                e.stopImmediatePropagation();
            }
        }, true);
        window.addEventListener("keydown", (e) => {
            if (wantLock && e.key !== "Escape" && document.pointerLockElement !== canvas) lock(canvas);
        }, true);
    }
    if (want) {
        if (document.pointerLockElement !== canvas) lock(canvas);
    } else if (document.pointerLockElement) {
        document.exitPointerLock();
    }
}

let picked = null;
export function horizon_pick_file(kind, accept) {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = async () => {
        const file = input.files && input.files[0];
        if (file) picked = [kind, new Uint8Array(await file.arrayBuffer())];
    };
    input.click();
}
export function horizon_take_picked() {
    const p = picked;
    picked = null;
    return p;
}
"#)]
extern "C" {
    fn horizon_pick_file(kind: &str, accept: &str);
    fn horizon_take_picked() -> JsValue;
    fn horizon_console(level: u32, message: &str);
    /// The browser's stack for this point, with the wasm frames' names.
    pub fn horizon_js_stack() -> String;
    fn horizon_pointer_lock(want: bool);
    fn horizon_pointer_lock_lost() -> bool;
    fn horizon_pointer_locked() -> bool;
}

static START: Mutex<Option<fn()>> = Mutex::new(None);
static ARGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Called from `main`, which wasm-bindgen runs as soon as the module loads:
/// the game itself starts once the page has installed its files and calls
/// `horizon_start`.
pub fn on_start(start: fn()) {
    *START.lock().unwrap_or_else(|e| e.into_inner()) = Some(start);
}

/// Adds the files of a tar archive under `root` in the virtual filesystem.
#[wasm_bindgen]
pub fn horizon_install_tar(root: &str, tar: &[u8]) -> Result<usize, JsValue> {
    crate::fs::install_tar(root, tar).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Lists files under `root` to fetch from `base` when first read (one
/// `path<TAB>length` per line).
#[wasm_bindgen]
pub fn horizon_install_manifest(root: &str, base: &str, manifest: &str) {
    crate::fs::install_manifest(root, base, manifest);
}

/// A file the page kept in the browser's storage, back in the virtual
/// filesystem before the game starts (`modified` in ms since 1970).
#[wasm_bindgen]
pub fn horizon_install_saved_file(path: &str, data: Vec<u8>, modified: f64) {
    crate::fs::install_saved_file(path, data, modified);
}

/// A folder the page kept in the browser's storage.
#[wasm_bindgen]
pub fn horizon_install_saved_dir(path: &str) {
    crate::fs::install_saved_dir(path);
}

/// The player's files changed since the last call, for the page to save:
/// `[path, "file", bytes]`, `[path, "dir" | "cleardir"]` (emptied first) or
/// `[path, "gone"]`, parents before children.
#[wasm_bindgen]
pub fn horizon_take_changes() -> js_sys::Array {
    let changes = js_sys::Array::new();
    for (path, change) in crate::fs::take_changes() {
        let entry = js_sys::Array::new();
        entry.push(&JsValue::from_str(&path));
        match change {
            crate::fs::Change::File(data) => {
                entry.push(&JsValue::from_str("file"));
                entry.push(&js_sys::Uint8Array::from(&data[..]));
            }
            crate::fs::Change::Dir { clear } => {
                entry.push(&JsValue::from_str(if clear { "cleardir" } else { "dir" }));
            }
            crate::fs::Change::Gone => {
                entry.push(&JsValue::from_str("gone"));
            }
        }
        changes.push(&entry);
    }
    changes
}

/// Starts the game registered by [`on_start`] with these arguments (what
/// native takes on its command line).
#[wasm_bindgen]
pub fn horizon_start(args: Vec<String>) {
    *ARGS.lock().unwrap_or_else(|e| e.into_inner()) = args;
    std::panic::set_hook(Box::new(|info| {
        horizon_console(1, &format!("Horizon panicked: {info}"));
    }));
    #[cfg(target_feature = "atomics")]
    crate::net::install();
    let start = START.lock().unwrap_or_else(|e| e.into_inner()).take();
    match start {
        Some(start) => start(),
        None => horizon_console(1, "Horizon: no game registered to start"),
    }
}

/// The arguments the page passed to [`horizon_start`].
pub(crate) fn args() -> Vec<String> {
    ARGS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A line in the browser's console (`log::Level` as a number: 1 error, 2
/// warning, more for the rest).
pub fn console(level: u32, message: &str) {
    horizon_console(level, message);
}

/// Locks the pointer to the game's canvas, or lets it go. Browsers lock it
/// only during a click or a key press: refused now, the canvas takes it on
/// the player's next click.
pub fn pointer_lock(want: bool) {
    horizon_pointer_lock(want);
}

/// Whether the browser holds the pointer for the game's canvas now. Until it
/// does, the pointer moves freely and its motion must not turn the view.
pub fn pointer_locked() -> bool {
    horizon_pointer_locked()
}

/// Whether the browser let the pointer go while the game held it (the
/// player pressed Escape, which the page never sees then) since the last call.
pub fn pointer_lock_lost() -> bool {
    horizon_pointer_lock_lost()
}

/// Asks the player for a file (`accept`: the file chooser's types). Browsers
/// open the chooser only during a click or a key press; the file comes later
/// from [`take_picked_file`], with `kind` to tell requests apart.
pub fn pick_file(kind: &str, accept: &str) {
    horizon_pick_file(kind, accept);
}

/// The file the player picked since the last call: its `kind` and bytes.
pub fn take_picked_file() -> Option<(String, Vec<u8>)> {
    let picked = horizon_take_picked();
    if picked.is_null() {
        return None;
    }
    let picked = js_sys::Array::from(&picked);
    let kind = picked.get(0).as_string()?;
    Some((kind, js_sys::Uint8Array::new(&picked.get(1)).to_vec()))
}
