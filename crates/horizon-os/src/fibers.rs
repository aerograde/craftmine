//! Threads on the page's plain build (no `+atomics`, where memory can't be
//! shared: a page that isn't cross-origin isolated, the offline file): each
//! thread is a green thread on the page's own. It runs its closure on a stack
//! of its own and gives the page back whenever it waits (`sleep`, a lock held
//! elsewhere, a channel or a condition with nothing yet), through WebAssembly
//! stack switching (JSPI): its wasm stack is suspended until a timer resumes
//! it. The page's thread itself never waits for one: a green thread runs only
//! once the page is back in the browser.
//!
//! Rust keeps part of each stack in linear memory (`__stack_pointer`, exported
//! by web/build.sh): each green thread gets its own region there, switched in
//! when it runs and out when it waits.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};
use wasm_bindgen::prelude::*;

pub use std::thread::Result;

#[wasm_bindgen(inline_js = r#"
// The page's own stack pointer, kept whenever no green thread runs, and the
// green threads' while they wait.
let sp, base, entry;
const saved = new Map();
let running = 0;
// What each green thread is doing, for the console: globalThis.horizonFibers.
const fibers = (globalThis.horizonFibers = new Map());

// The module imports `horizon_fiber_suspend` straight from this one when it
// can; when the glue wraps it in a JS shim instead (which can't suspend
// wasm), the shim is replaced as the module is instantiated.
function suspending(imports) {
    for (const space of Object.values(imports ?? {})) {
        for (const name of Object.keys(space)) {
            if (name.startsWith("__wbg_horizon_fiber_suspend_")) space[name] = horizon_fiber_suspend;
        }
    }
    return imports;
}
const instantiate = WebAssembly.instantiate;
WebAssembly.instantiate = (module, imports) => instantiate(module, suspending(imports));
const instantiateStreaming = WebAssembly.instantiateStreaming;
if (instantiateStreaming) {
    WebAssembly.instantiateStreaming = (source, imports) => instantiateStreaming(source, suspending(imports));
}

// A task after the browser's own work, without timers' 4 ms floor.
const channel = new MessageChannel();
const queued = [];
channel.port1.onmessage = () => queued.shift()();
function soon(f) {
    queued.push(f);
    channel.port2.postMessage(0);
}

async function suspend(ms) {
    const id = running;
    const fiber = fibers.get(id);
    fiber.waits += 1;
    fiber.waiting = ms;
    saved.set(id, sp.value);
    sp.value = base;
    running = 0;
    await new Promise((resolve) => (ms > 0 ? setTimeout(resolve, ms) : soon(resolve)));
    sp.value = saved.get(id);
    saved.delete(id);
    running = id;
    fiber.waiting = null;
}

export const horizon_fiber_suspend = typeof WebAssembly.Suspending === "function"
    ? new WebAssembly.Suspending(suspend)
    : () => { throw new Error("this browser can't switch WebAssembly stacks"); };

export function horizon_fiber_supported() {
    return typeof WebAssembly.Suspending === "function" && typeof WebAssembly.promising === "function";
}

export function horizon_fiber_start(exports, id, top, name) {
    fibers.set(id, { name, waits: 0, waiting: null });
    sp ??= exports.__stack_pointer;
    entry ??= WebAssembly.promising(exports.horizon_fiber_entry);
    soon(() => {
        // Nothing of the page's runs now: its stack pointer is at rest.
        base = sp.value;
        sp.value = top;
        running = id;
        entry(id).then(
            () => {
                sp.value = base;
                fibers.delete(id);
            },
            (e) => {
                sp.value = base;
                fibers.delete(id);
                console.error(`Horizon: green thread ${name} failed: ${e}`);
            });
        if (running === id) running = 0;
        sp.value = base;
    });
}
"#)]
extern "C" {
    fn horizon_fiber_supported() -> bool;
    fn horizon_fiber_start(exports: JsValue, id: u32, top: u32, name: &str);
    fn horizon_fiber_suspend(ms: f64);
}

type Main = Box<dyn FnOnce() + Send>;

/// The threads that run as green threads (by name, or its start): single
/// player's server and its connection through memory, whose waits all go
/// through horizon-os, which gives the page back, the server's region writer,
/// and its loads that never wait. std's own waits (`recv()`,
/// `Condvar::wait`, `park`) can't: without threads they return at once and a
/// loop on them would hold the page forever. Other threads fail to start, as
/// std's do, and the game does their work on the page instead.
///
/// The world's own opening is in here with them: a world that never opened
/// left the loading screen up forever, which read as a hang at "Loading
/// terrain...". Its threads (`World initialization`, the server's data and
/// loot preloads, the generator's loads) hand their work to the page while
/// they wait, so the world opens as it does on native, only slower.
const GREEN: &[&str] = &[
    "Server thread",
    "horizon-local-conn",
    "horizon-lan-out-",
    "horizon-net",
    "Profile fetcher",
    "Worldgen loader",
    "horizon-region-writer",
    "World initialization",
    "World data preload",
    "Server data preload",
    "Loot preload",
    // Loads the world tables the title waits behind: on the page's own thread
    // it was a many-second freeze before the first frame.
    "world-rest-loader",
    // A shader pack's GLSL translation: seconds of CPU that must stay off the frame thread.
    "Shader pack translation",
    // Play Nearby: the service loop, one thread per MQTT broker, and the guest,
    // host, LAN and pump threads of a session. Without these the service
    // silently stayed off in the browser (`Service::start` reported a disabled
    // snapshot, with no log line saying why) — which is why nobody was ever
    // seen nearby on the web.
    "horizon-nearby",
    "horizon-friends-auth",
    "horizon-friends-listen",
];

struct Fiber {
    main: Option<Main>,
    /// Its part of linear memory for Rust's stack.
    _stack: Vec<u128>,
}

thread_local! {
    static FIBERS: RefCell<BTreeMap<u32, Fiber>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT: Cell<u32> = const { Cell::new(1) };
    /// Green threads that returned: their stacks are freed by the next spawn,
    /// not while they still run on them.
    static DONE: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// The green thread running now, 0 on the page's own.
    static CURRENT: Cell<u32> = const { Cell::new(0) };
}

/// Whether this code runs on a green thread, which may wait.
pub fn in_fiber() -> bool {
    CURRENT.with(Cell::get) != 0
}

/// Gives the page back for at least `ms` (0: until the browser has done its
/// own work). On a green thread only.
fn suspend(ms: f64) {
    // The page and the other green threads run meanwhile, as themselves.
    let id = CURRENT.with(|c| c.replace(0));
    horizon_fiber_suspend(ms);
    CURRENT.with(|c| c.set(id));
}

/// Lets the page and the other green threads run a while; at once on the page.
pub fn yield_now() {
    if in_fiber() {
        suspend(0.0);
    }
}

/// Run by the page's scheduler, on the green thread's own stack.
#[wasm_bindgen]
pub fn horizon_fiber_entry(id: u32) {
    CURRENT.with(|c| c.set(id));
    let main = FIBERS.with(|f| f.borrow_mut().get_mut(&id).and_then(|f| f.main.take()));
    if let Some(main) = main {
        main();
    }
    CURRENT.with(|c| c.set(0));
    DONE.with(|d| d.borrow_mut().push(id));
}

#[derive(Clone, Debug)]
pub struct Thread {
    name: Option<Arc<str>>,
}

impl Thread {
    /// Green threads wait by looking again shortly: there is nothing to wake.
    pub fn unpark(&self) {}
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

pub struct JoinHandle<T> {
    result: Arc<Mutex<Option<T>>>,
    thread: Thread,
}

impl<T> JoinHandle<T> {
    /// The thread's result. The page's thread can't wait for one still
    /// running (it runs only once the page is back in the browser): `Err` then.
    pub fn join(self) -> Result<T> {
        loop {
            if let Some(r) = self.result.lock().unwrap_or_else(|e| e.into_inner()).take() {
                return Ok(r);
            }
            if !in_fiber() {
                crate::web::console(
                    1,
                    &format!(
                        "Horizon: the page can't wait for {}, still running",
                        self.thread.name().unwrap_or("a green thread")
                    ),
                );
                return Err(Box::new("still running"));
            }
            suspend(1.0);
        }
    }
    pub fn is_finished(&self) -> bool {
        // `lock`, not `lock()`: a green thread may hold this while the page asks, and std's
        // own wait traps there ("Atomics.wait cannot be called in this context") — winit then
        // swallows it and the frame loop stops with no message.
        crate::sync::lock(&self.result).is_ok_and(|result| result.is_some())
    }
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

#[derive(Debug, Default)]
pub struct Builder {
    name: Option<String>,
    stack_size: Option<usize>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }
    pub fn stack_size(mut self, size: usize) -> Self {
        self.stack_size = Some(size);
        self
    }
    pub fn spawn<F, T>(self, f: F) -> io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let name = self.name.as_deref().unwrap_or("an unnamed thread");
        if !GREEN.iter().any(|green| name.starts_with(green)) {
            crate::web::console(2, &format!("Horizon: no thread on the page for {name}"));
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("no thread on the page for {name}"),
            ));
        }
        if !horizon_fiber_supported() {
            return Err(io::Error::other(format!(
                "no thread for {name}: this browser can't switch WebAssembly stacks (JSPI)"
            )));
        }
        for id in DONE.with(|d| std::mem::take(&mut *d.borrow_mut())) {
            FIBERS.with(|f| f.borrow_mut().remove(&id));
        }
        let result = Arc::new(Mutex::new(None));
        let theirs = result.clone();
        let main: Main = Box::new(move || {
            let r = f();
            *theirs.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
        // Twice std's default for a spawned thread: nothing guards its end.
        let size = self.stack_size.unwrap_or(2 << 20).max(4 << 20);
        let stack = vec![0u128; size / 16];
        // Rust's stack grows down from the end, 16-byte aligned.
        let top = stack.as_ptr_range().end as u32;
        let id = NEXT.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        FIBERS.with(|f| {
            f.borrow_mut().insert(
                id,
                Fiber {
                    main: Some(main),
                    _stack: stack,
                },
            )
        });
        horizon_fiber_start(wasm_bindgen::exports(), id, top, name);
        Ok(JoinHandle {
            result,
            thread: Thread {
                name: self.name.map(Into::into),
            },
        })
    }
}

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).expect("failed to spawn thread")
}

/// Waits on a green thread; at once on the page, whose thread may not wait.
pub fn sleep(dur: Duration) {
    if in_fiber() {
        suspend(dur.as_secs_f64() * 1000.0);
    }
}

/// One core: the green threads take turns on the page's.
pub fn available_parallelism() -> io::Result<NonZeroUsize> {
    Ok(NonZeroUsize::MIN)
}
