//! The operating system as the game sees it: files, clocks and the process's
//! folder. On native these are std's own; in the browser, where std has no
//! filesystem and its clocks panic, they are a virtual filesystem (backed by
//! files fetched from the page's origin) and the browser's clocks, with the
//! same API so call sites read the same on both.

pub mod env;
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
pub mod fibers;

/// `navigator.deviceMemory`, in GiB, as the page's bootstrap records it; 0 when
/// the browser does not report it. Native builds have no such hint.
pub fn device_memory_gib() -> f32 {
    #[cfg(target_arch = "wasm32")]
    {
        js_number("horizonDeviceMemory") as f32
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

/// The CPU threads the device actually has, not the page's green-thread count.
pub fn hardware_threads() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        let reported = js_number("horizonHardwareConcurrency");
        if reported > 0.0 {
            return reported as usize;
        }
    }
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// A public address the page's bootstrap looked up over HTTP, for the browser
/// builds where WebRTC STUN is unavailable or blocked. Never used by the game
/// unless Nearby is opened.
pub fn public_ip_hint() -> Option<std::net::IpAddr> {
    #[cfg(target_arch = "wasm32")]
    {
        let value = js_sys::Reflect::get(
            &js_sys::global(),
            &wasm_bindgen::JsValue::from_str("horizonPublicIp"),
        )
        .ok()?;
        value.as_string()?.parse().ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

#[cfg(target_arch = "wasm32")]
fn js_number(name: &str) -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &wasm_bindgen::JsValue::from_str(name))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}
pub mod fs;
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
pub mod net;
#[cfg(target_arch = "wasm32")]
pub mod web;
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
pub mod workers;

/// Waits for `future`. On native, as long as it takes; on the web, where the
/// page's only thread can't wait for the browser, `None` unless the future is
/// ready on its first poll.
pub fn block_on<F: std::future::Future>(future: F) -> Option<F::Output> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(pollster::block_on(future))
    }
    #[cfg(target_arch = "wasm32")]
    {
        use std::task::{Context, Poll, Waker};
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        }
    }
}

/// std's clocks on native; the browser's `performance.now()` and `Date.now()`
/// on the web, where `std::time::Instant::now()` panics.
pub mod time {
    pub use std::time::Duration;
    pub use web_time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

    /// The same moment as std's `SystemTime`, for libraries that take std's
    /// (chrono): the identity on native.
    pub fn to_std(t: SystemTime) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + t.duration_since(UNIX_EPOCH).unwrap_or_default()
    }
}

/// std's threads. On the page, scoped threads run one after another on the
/// caller's and sleeping on the page's thread returns at once (it may not
/// block). The shared-memory build (`+atomics`) starts other threads as Web
/// Workers ([`crate::workers`]); the plain build as green threads on the
/// page's ([`crate::fibers`]).
pub mod thread {
    pub use std::thread::*;

    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    pub use crate::fibers::{
        Builder, JoinHandle, Thread, available_parallelism, sleep, spawn, yield_now,
    };
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    pub use crate::workers::{Builder, JoinHandle, Thread, available_parallelism, sleep, spawn};
    #[cfg(target_arch = "wasm32")]
    pub use sequential::{Scope, ScopedJoinHandle, scope};

    #[cfg(target_arch = "wasm32")]
    mod sequential {
        use std::marker::PhantomData;

        pub struct Scope<'scope, 'env: 'scope> {
            _scope: PhantomData<&'scope mut &'scope ()>,
            _env: PhantomData<&'env mut &'env ()>,
        }

        pub struct ScopedJoinHandle<'scope, T> {
            result: T,
            _scope: PhantomData<&'scope ()>,
        }

        impl<'scope, 'env> Scope<'scope, 'env> {
            /// Runs `f` now and keeps what it returns for `join`.
            pub fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
            where
                F: FnOnce() -> T + Send + 'scope,
                T: Send + 'scope,
            {
                ScopedJoinHandle {
                    result: f(),
                    _scope: PhantomData,
                }
            }
        }

        impl<T> ScopedJoinHandle<'_, T> {
            pub fn join(self) -> std::thread::Result<T> {
                Ok(self.result)
            }
            pub fn is_finished(&self) -> bool {
                true
            }
        }

        pub fn scope<'env, F, T>(f: F) -> T
        where
            F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
        {
            f(&Scope {
                _scope: PhantomData,
                _env: PhantomData,
            })
        }
    }
}

/// std's timed waits that read std's clock, which panics on the web: there
/// they time themselves with the browser's. Never on the page's own thread,
/// which may not wait: it only checks once. A green thread (the plain build's,
/// [`crate::fibers`]) gives the page back while it waits, its lock let go.
pub mod sync {
    use crate::time::Duration;
    use std::sync::{
        Condvar, LockResult, Mutex, MutexGuard, RwLock, RwLockWriteGuard,
        mpsc::{Receiver, RecvError, RecvTimeoutError},
    };

    /// `mutex.lock()`. The page's thread, which may not wait (the browser
    /// traps it), spins until the lock is free instead: use it for locks the
    /// page shares with workers, which hold them only briefly. A green thread
    /// lets the others run until it is free.
    pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> LockResult<MutexGuard<'_, T>> {
        #[cfg(target_arch = "wasm32")]
        if !can_wait() || cfg!(not(target_feature = "atomics")) {
            loop {
                match mutex.try_lock() {
                    Ok(guard) => return Ok(guard),
                    Err(std::sync::TryLockError::Poisoned(e)) => return Err(e),
                    Err(std::sync::TryLockError::WouldBlock) => busy(),
                }
            }
        }
        mutex.lock()
    }

    /// `lock.write()`, spinning on the page's thread as [`lock`] does.
    pub fn write<T: ?Sized>(lock: &RwLock<T>) -> LockResult<RwLockWriteGuard<'_, T>> {
        #[cfg(target_arch = "wasm32")]
        if !can_wait() || cfg!(not(target_feature = "atomics")) {
            loop {
                match lock.try_write() {
                    Ok(guard) => return Ok(guard),
                    Err(std::sync::TryLockError::Poisoned(e)) => return Err(e),
                    Err(std::sync::TryLockError::WouldBlock) => busy(),
                }
            }
        }
        lock.write()
    }

    /// A lock held elsewhere: spin on the page while a worker holds it; on the
    /// plain build, where only a waiting green thread can hold it, let it run.
    #[cfg(target_arch = "wasm32")]
    fn busy() {
        #[cfg(target_feature = "atomics")]
        std::hint::spin_loop();
        #[cfg(not(target_feature = "atomics"))]
        if can_wait() {
            crate::fibers::yield_now();
        } else {
            panic!("the page's thread needs a lock a waiting green thread holds");
        }
    }

    /// `rx.recv()`. A green thread looks again shortly, the others running
    /// meanwhile.
    pub fn recv<T>(rx: &Receiver<T>) -> Result<T, RecvError> {
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        loop {
            use std::sync::mpsc::TryRecvError;
            match rx.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {
                    assert!(can_wait(), "the page's thread may not wait");
                    crate::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
        rx.recv()
    }

    /// `rx.recv_timeout(timeout)`.
    pub fn recv_timeout<T>(rx: &Receiver<T>, timeout: Duration) -> Result<T, RecvTimeoutError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            rx.recv_timeout(timeout)
        }
        #[cfg(target_arch = "wasm32")]
        {
            use std::sync::mpsc::TryRecvError;
            let deadline = crate::time::Instant::now() + timeout;
            loop {
                match rx.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                    Err(TryRecvError::Empty) => {}
                }
                if !can_wait() || crate::time::Instant::now() >= deadline {
                    return Err(RecvTimeoutError::Timeout);
                }
                // A channel can't be waited on with a deadline here: look again shortly.
                crate::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// `rx.recv_timeout(timeout)` for a reply that a thread already running
    /// sends shortly. The page's thread, which may not wait, looks again
    /// until the deadline instead of giving up at once; on the plain build,
    /// whose green threads run only once the page is back in the browser, it
    /// looks once.
    pub fn recv_reply<T>(rx: &Receiver<T>, timeout: Duration) -> Result<T, RecvTimeoutError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            rx.recv_timeout(timeout)
        }
        #[cfg(target_arch = "wasm32")]
        {
            if can_wait() || cfg!(not(target_feature = "atomics")) {
                return recv_timeout(rx, timeout);
            }
            use std::sync::mpsc::TryRecvError;
            let deadline = crate::time::Instant::now() + timeout;
            loop {
                match rx.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                    Err(TryRecvError::Empty) if crate::time::Instant::now() >= deadline => {
                        return Err(RecvTimeoutError::Timeout);
                    }
                    Err(TryRecvError::Empty) => std::hint::spin_loop(),
                }
            }
        }
    }

    /// `condvar.wait_while(guard, condition)` on `mutex`'s guard, without the
    /// poisoning.
    pub fn wait_while<'a, T, F>(
        mutex: &'a Mutex<T>,
        condvar: &Condvar,
        guard: MutexGuard<'a, T>,
        condition: F,
    ) -> MutexGuard<'a, T>
    where
        F: FnMut(&mut T) -> bool,
    {
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        {
            let _ = condvar;
            let (mut guard, mut condition) = (guard, condition);
            while condition(&mut *guard) {
                guard = wait_green(mutex, guard, Duration::from_millis(1));
            }
            guard
        }
        #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
        {
            let _ = mutex;
            condvar
                .wait_while(guard, condition)
                .unwrap_or_else(|e| e.into_inner())
        }
    }

    /// `condvar.wait_timeout_while(guard, timeout, condition)` on `mutex`'s
    /// guard, without the poisoning: the guard, and whether it timed out.
    pub fn wait_timeout_while<'a, T, F>(
        mutex: &'a Mutex<T>,
        condvar: &Condvar,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
        condition: F,
    ) -> (MutexGuard<'a, T>, bool)
    where
        F: FnMut(&mut T) -> bool,
    {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = mutex;
            let (guard, result) = condvar
                .wait_timeout_while(guard, timeout, condition)
                .unwrap_or_else(|e| e.into_inner());
            (guard, result.timed_out())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (mut guard, mut condition) = (guard, condition);
            let deadline = crate::time::Instant::now() + timeout;
            while condition(&mut *guard) {
                let left = deadline.saturating_duration_since(crate::time::Instant::now());
                if !can_wait() || left.is_zero() {
                    return (guard, true);
                }
                #[cfg(target_feature = "atomics")]
                {
                    let _ = mutex;
                    // std's plain timed wait needs no clock.
                    guard = condvar
                        .wait_timeout(guard, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                #[cfg(not(target_feature = "atomics"))]
                {
                    let _ = condvar;
                    guard = wait_green(mutex, guard, left.min(Duration::from_millis(1)));
                }
            }
            (guard, false)
        }
    }

    /// A green thread's wait: the lock let go, the page and the other green
    /// threads run a while, then the lock again. Nothing can notify it, so it
    /// looks again after `dur`.
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    fn wait_green<'a, T>(
        mutex: &'a Mutex<T>,
        guard: MutexGuard<'a, T>,
        dur: Duration,
    ) -> MutexGuard<'a, T> {
        assert!(can_wait(), "the page's thread may not wait");
        drop(guard);
        crate::thread::sleep(dur);
        lock(mutex).unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(target_arch = "wasm32")]
    fn can_wait() -> bool {
        #[cfg(target_feature = "atomics")]
        {
            crate::workers::in_worker()
        }
        #[cfg(not(target_feature = "atomics"))]
        {
            crate::fibers::in_fiber()
        }
    }
}

/// The process. The page has one game and no process id: 1.
pub mod process {
    #[cfg(not(target_arch = "wasm32"))]
    pub use std::process::id;

    #[cfg(target_arch = "wasm32")]
    pub fn id() -> u32 {
        1
    }
}
