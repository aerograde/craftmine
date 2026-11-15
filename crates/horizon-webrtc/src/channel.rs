//! The channel's queue state, written by the peer thread and read by the game.
use crate::peer::Command;
use horizon_os::time::Duration;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{Arc, Condvar, Mutex, mpsc::Sender},
};

/// The most game bytes waiting to be written to the data channel.
const OUTBOUND: usize = 8 << 20;
/// The most bytes received and not read yet.
const INBOUND: usize = 16 << 20;

#[derive(Default)]
struct State {
    inbound: VecDeque<u8>,
    outbound: VecDeque<Vec<u8>>,
    timeout: Option<Duration>,
    closed: bool,
}

pub struct Inner {
    state: Mutex<State>,
    ready: Condvar,
}
impl Inner {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
        }
    }
    pub(crate) fn receive(&self, data: &[u8]) -> bool {
        let mut state = horizon_os::sync::lock(&self.state).unwrap();
        if state.closed || state.inbound.len() + data.len() > INBOUND {
            return false;
        }
        state.inbound.extend(data);
        self.ready.notify_all();
        true
    }
    pub(crate) fn take(&self) -> Option<Vec<u8>> {
        let mut state = horizon_os::sync::lock(&self.state).unwrap();
        state.outbound.pop_front()
    }
    pub(crate) fn close(&self) {
        horizon_os::sync::lock(&self.state).unwrap().closed = true;
        self.ready.notify_all();
    }
    pub(crate) fn is_closed(&self) -> bool {
        horizon_os::sync::lock(&self.state).unwrap().closed
    }
}

/// One end of a data channel, as a blocking byte stream.
pub struct Channel {
    pub(crate) inner: Arc<Inner>,
    wake: Option<Sender<Command>>,
}
impl Clone for Channel {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            wake: self.wake.clone(),
        }
    }
}
impl Channel {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
            wake: None,
        }
    }
    pub(crate) fn with_wake(mut self, wake: Sender<Command>) -> Self {
        self.wake = Some(wake);
        self
    }
    pub fn try_clone(&self) -> Self {
        self.clone()
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) {
        horizon_os::sync::lock(&self.inner.state).unwrap().timeout = timeout;
    }
    pub fn shutdown(&self) {
        self.inner.close();
        if let Some(wake) = &self.wake {
            let _ = wake.send(Command::Close);
        }
    }
}
impl Read for Channel {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let state = &self.inner.state;
        let mut guard = horizon_os::sync::lock(state).unwrap();
        loop {
            if !guard.inbound.is_empty() {
                let n = out.len().min(guard.inbound.len());
                for (o, b) in out.iter_mut().zip(guard.inbound.drain(..n)) {
                    *o = b;
                }
                return Ok(n);
            }
            if guard.closed {
                return Ok(0);
            }
            guard = match guard.timeout {
                None => horizon_os::sync::wait_while(state, &self.inner.ready, guard, |s| {
                    s.inbound.is_empty() && !s.closed
                }),
                Some(timeout) => {
                    let (guard, timed_out) = horizon_os::sync::wait_timeout_while(
                        state,
                        &self.inner.ready,
                        guard,
                        timeout,
                        |s| s.inbound.is_empty() && !s.closed,
                    );
                    if timed_out {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    guard
                }
            };
        }
    }
}
impl Write for Channel {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut state = horizon_os::sync::lock(&self.inner.state).unwrap();
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let queued: usize = state.outbound.iter().map(Vec::len).sum();
        if queued + data.len() > OUTBOUND {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the data channel is not keeping up",
            ));
        }
        // A message transport: each write is one channel message, cut by the
        // peer thread into 16 KiB pieces.
        state.outbound.push_back(data.to_vec());
        if let Some(wake) = &self.wake {
            let _ = wake.send(Command::Wake);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
