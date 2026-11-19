//! Threads on the page's shared-memory build (`+atomics`): each thread is a
//! Web Worker that instantiates the same module on the same memory, then runs
//! the closure it was handed. Workers always start from the page: a worker's
//! own JS stays busy running its thread, so it asks the page to start one.
//!
//! The page's thread itself may never wait (browsers trap
//! `memory.atomic.wait32` there): `sleep` returns at once, and `join` on the
//! page spins instead of parking.

use std::{
    cell::Cell,
    io,
    num::NonZeroUsize,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use wasm_bindgen::prelude::*;

pub use std::thread::Result;

#[wasm_bindgen(inline_js = r#"
let pageModule, pageMemory;
// The device facts the page recorded (`web/main.js`). A worker has its own
// `globalThis`, so they are copied across with every spawn: the worldgen and
// meshing threads read them for their defaults, and Nearby reads the public IP
// to pick its room. Without this they are undefined in every worker.
const pageFacts = ["horizonDeviceMemory", "horizonHardwareConcurrency", "horizonPublicIp"];
function facts() {
    const out = {};
    for (const key of pageFacts) if (globalThis[key] !== undefined) out[key] = globalThis[key];
    return out;
}
function start(ptr, name, stack) {
    const worker = new Worker(new URL("worker.js", location.href), { type: "module", name });
    // A thread this worker starts: the page starts it. Its sockets live on
    // the page too (net.rs).
    worker.onmessage = ({ data }) =>
        data.socket ? globalThis.horizonSocket(data) : start(data.ptr, data.name, data.stack);
    worker.onerror = (e) => console.error(`Horizon: thread ${name} failed: ${e.message ?? e}`);
    worker.postMessage({ module: pageModule, memory: pageMemory, ptr, name, stack, ...facts() });
}
export function horizon_spawn_worker(module, memory, ptr, name, stack) {
    if (typeof window === "undefined") {
        self.postMessage({ ptr, name, stack, ...facts() });
    } else {
        pageModule = module;
        pageMemory = memory;
        start(ptr, name, stack);
    }
}
export function horizon_hardware_concurrency() {
    return navigator.hardwareConcurrency || 1;
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn horizon_spawn_worker(
        module: JsValue,
        memory: JsValue,
        ptr: u32,
        name: &str,
        stack: u32,
    ) -> std::result::Result<(), JsValue>;
    fn horizon_hardware_concurrency() -> u32;
}

thread_local! {
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether this code runs on a worker (true) or on the page's own thread.
pub fn in_worker() -> bool {
    IN_WORKER.with(Cell::get)
}

type Main = Box<dyn FnOnce() + Send>;

/// Run by `worker.js` once the module is instantiated on the shared memory.
#[wasm_bindgen]
pub fn horizon_worker_entry(ptr: u32) {
    IN_WORKER.with(|w| w.set(true));
    // SAFETY: `ptr` is the `Box<Main>` that `Builder::spawn` leaked for this worker.
    let main = unsafe { Box::from_raw(ptr as *mut Main) };
    main();
}

/// A spawned thread, usable before its worker has started (which needs the
/// page to get back to the browser first): an unpark sent early is kept and
/// delivered when it starts.
#[derive(Clone, Debug)]
pub struct Thread {
    started: Arc<Started>,
}

#[derive(Debug, Default)]
struct Started {
    thread: OnceLock<std::thread::Thread>,
    unparked: AtomicBool,
}

impl Thread {
    pub fn unpark(&self) {
        match self.started.thread.get() {
            Some(t) => t.unpark(),
            None => {
                self.started.unparked.store(true, Ordering::Release);
                // Started meanwhile: it may have missed the flag.
                if let Some(t) = self.started.thread.get() {
                    t.unpark();
                }
            }
        }
    }
    pub fn name(&self) -> Option<&str> {
        self.started.thread.get().and_then(|t| t.name())
    }
}

struct Packet<T> {
    result: Mutex<Option<T>>,
    done: Condvar,
    thread: Thread,
}

pub struct JoinHandle<T> {
    packet: Arc<Packet<T>>,
}

impl<T> JoinHandle<T> {
    /// The thread's result. A thread that panicked aborted its worker, so this
    /// never returns `Err`; it waits forever for it instead.
    pub fn join(self) -> Result<T> {
        if in_worker() {
            let mut result = self.packet.result.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(r) = result.take() {
                    return Ok(r);
                }
                result = self
                    .packet
                    .done
                    .wait(result)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
        loop {
            if let Ok(mut result) = self.packet.result.try_lock()
                && let Some(r) = result.take()
            {
                return Ok(r);
            }
            std::hint::spin_loop();
        }
    }
    pub fn is_finished(&self) -> bool {
        self.packet
            .result
            .try_lock()
            .is_ok_and(|result| result.is_some())
    }
    pub fn thread(&self) -> &Thread {
        &self.packet.thread
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
        let packet = Arc::new(Packet {
            result: Mutex::new(None),
            done: Condvar::new(),
            thread: Thread {
                started: Arc::default(),
            },
        });
        let theirs = packet.clone();
        let main: Main = Box::new(move || {
            let started = &theirs.thread.started;
            let _ = started.thread.set(std::thread::current());
            if started.unparked.load(Ordering::Acquire) {
                std::thread::current().unpark();
            }
            let r = f();
            *theirs.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            theirs.done.notify_all();
        });
        let ptr = Box::into_raw(Box::new(main));
        // std's default for a spawned thread.
        let stack = self.stack_size.unwrap_or(2 << 20) as u32;
        let name = self.name.as_deref().unwrap_or("thread");
        if let Err(e) = horizon_spawn_worker(
            wasm_bindgen::module(),
            wasm_bindgen::memory(),
            ptr as u32,
            name,
            stack,
        ) {
            // SAFETY: the worker never started, so the box is still ours.
            drop(unsafe { Box::from_raw(ptr) });
            return Err(io::Error::other(format!("no worker for {name}: {e:?}")));
        }
        Ok(JoinHandle { packet })
    }
}

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).expect("failed to spawn thread")
}

/// std's on a worker; at once on the page, whose thread may not wait.
pub fn sleep(dur: std::time::Duration) {
    if in_worker() {
        std::thread::sleep(dur);
    }
}

/// The browser's `navigator.hardwareConcurrency` (std has no answer on the web).
pub fn available_parallelism() -> io::Result<NonZeroUsize> {
    Ok(NonZeroUsize::new(horizon_hardware_concurrency() as usize).unwrap_or(NonZeroUsize::MIN))
}
