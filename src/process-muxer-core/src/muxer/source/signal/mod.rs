use mio::event::Source;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::exfiltrator::SignalOnly;
use signal_hook_mio::v0_8::{Pending, Signals};
use std::fmt::Debug;
use std::io;

use crate::muxer::source::SourceInstruction;

use super::EventStream;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Signal {
    Hangup,
    Interrupt,
    Terminate,
}

#[derive(Debug)]
enum State {
    Waiting,
    Draining(Pending<SignalOnly>),
}

pub struct SignalSource {
    signals: Signals,
    state: State,
}

impl Debug for SignalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalSource")
            .field("signals", &"<opaque>")
            .field("state", &self.state)
            .finish()
    }
}

impl Source for SignalSource {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        self.signals.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        self.signals.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        self.signals.deregister(registry)
    }
}

impl SignalSource {
    pub fn new() -> io::Result<Self> {
        let signals = Signals::new([SIGHUP, SIGINT, SIGTERM])?;
        let state = State::Waiting;
        let res = Self { signals, state };
        Ok(res)
    }

    pub fn next(&mut self) -> EventStream<Signal> {
        loop {
            match &mut self.state {
                State::Waiting => self.state = State::Draining(self.signals.pending()),
                State::Draining(ref mut xs) => match xs.next() {
                    Some(signum) => {
                        let sig = match signum {
                            SIGHUP => Signal::Hangup,
                            SIGINT => Signal::Interrupt,
                            SIGTERM => Signal::Interrupt,
                            _ => unreachable!("todo"),
                        };
                        return EventStream::Emit(sig);
                    }
                    None => {
                        self.state = State::Waiting;
                        return EventStream::Drained(SourceInstruction::Reregister);
                    }
                },
            }
        }
    }
}
