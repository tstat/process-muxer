mod process;
pub(crate) mod source;
pub use process::pid::Pid;
use source::termination::ChildTerminationSource;
use std::{
    cell::Cell,
    collections::BTreeMap,
    io::{self, BufRead, ErrorKind},
    mem,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus},
    rc::Rc,
};

use mio::{
    event::{self, Source},
    Events, Interest, Poll, Token,
};
use slab::Slab;

pub use self::source::childout::FdTag;
use self::source::{childout::ChildOut, EventStream, SourceInstruction};

#[cfg(feature = "signals")]
use source::signal::{Signal, SignalSource};

/// A handle to a child process that was spawned with `Muxer`.
pub struct ChildInfo {
    pub pid: Pid,
    pub stdin: Option<ChildStdin>,
    prog_path: Rc<PathBuf>,
    exit_status: Rc<Cell<Option<ExitStatus>>>,
}

impl ChildInfo {
    pub fn program(&self) -> &Path {
        &self.prog_path
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status.get()
    }
}

/// A user-facing event emitted by the `Muxer`
#[derive(Debug)]
pub enum Event<'a> {
    ChildTerminated {
        pid: Pid,
        prog_path: &'a Path,
        exit_status: ExitStatus,
    },
    ChildWrote {
        pid: Pid,
        prog_path: &'a Path,
        tag: FdTag,
        line: &'a str,
    },
    FdClosed {
        pid: Pid,
        prog_path: &'a Path,
        tag: FdTag,
    },
    #[cfg(feature = "signals")]
    SignalReceived { signal: Signal },
}

/// A process Muxer
pub struct Muxer {
    poll: Poll,
    events: Events,
    children: BTreeMap<Pid, MuxerChild>,
    fds: Slab<EventSource>,
    state: State,
    wait_buffer: Vec<(Pid, Rc<PathBuf>, ExitStatus)>,
    // We don't need this field, an index into "events" would do, but the Events
    // type only exposes an iterator over references
    pending_events: Vec<event::Event>,
}

impl Muxer {
    pub fn new() -> io::Result<Self> {
        let mut res = Self {
            poll: Poll::new()?,
            wait_buffer: Vec::new(),
            events: Events::with_capacity(1024),
            children: BTreeMap::new(),
            fds: Slab::new(),
            state: State::Awaiting,
            pending_events: Vec::new(),
        };

        let wait_source = ChildTerminationSource::new()?;
        res.register(EventSource::ChildTerminated(wait_source));
        #[cfg(feature = "signals")]
        {
            let signal_source = SignalSource::new()?;
            res.register(EventSource::ReceivedSignal(signal_source));
        }
        Ok(res)
    }

    pub fn pids(&self) -> impl Iterator<Item = &Pid> {
        self.children.keys()
    }

    pub fn spawn(&mut self, mut cmd: Command) -> io::Result<ChildInfo> {
        let prog_path = PathBuf::from(cmd.get_program());

        let mut child = cmd.spawn()?;
        let pid = Pid { inner: child.id() };
        let registry = self.poll.registry();
        let prog_path = Rc::new(prog_path);

        let child_info = ChildInfo {
            pid,
            stdin: child.stdin.take(),
            prog_path: prog_path.clone(),
            exit_status: Rc::new(Cell::new(None)),
        };

        if let Some(stdout) = child.stdout.take() {
            let prog_path = prog_path.clone();
            let mut stdout = ChildOut::from_pipe(stdout, pid, prog_path);
            let entry = self.fds.vacant_entry();
            registry.register(&mut stdout, Token(entry.key()), Interest::READABLE)?;
            entry.insert(EventSource::ReadableChild(stdout));
        }

        if let Some(stderr) = child.stderr.take() {
            let prog_path = prog_path.clone();
            let mut stderr = ChildOut::from_pipe(stderr, pid, prog_path);
            let entry = self.fds.vacant_entry();
            registry.register(&mut stderr, Token(entry.key()), Interest::READABLE)?;
            entry.insert(EventSource::ReadableChild(stderr));
        }

        let muxer_child = MuxerChild {
            child,
            prog_path: prog_path.clone(),
            exit_status: child_info.exit_status.clone(),
        };

        self.children.insert(pid, muxer_child);
        Ok(child_info)
    }

    fn register(&mut self, mut evsrc: EventSource) {
        let entry = self.fds.vacant_entry();
        evsrc
            .register(self.poll.registry(), Token(entry.key()), Interest::READABLE)
            .unwrap();
        entry.insert(evsrc);
    }

    fn reregister(&mut self, mut evsrc: EventSource) {
        let entry = self.fds.vacant_entry();
        evsrc
            .reregister(self.poll.registry(), Token(entry.key()), Interest::READABLE)
            .unwrap();
        entry.insert(evsrc);
    }

    fn deregister(&mut self, mut evsrc: EventSource) {
        evsrc.deregister(self.poll.registry()).unwrap();
    }

    pub fn pump<R, F>(&mut self, mut func: F) -> R
    where
        F: FnMut(Event) -> Option<R>,
    {
        let mut state = mem::replace(&mut self.state, State::Awaiting);
        let (state, event) = loop {
            match state {
                State::Awaiting => match self.pending_events.pop() {
                    None => {
                        // fill our events buffer
                        loop {
                            match self.poll.poll(&mut self.events, None) {
                                Ok(()) => break,
                                Err(e) => match e.kind() {
                                    // if our poll is interrupted by a
                                    // system call then retry
                                    ErrorKind::Interrupted => continue,
                                    _ => panic!("Unexpected error during poll: {e}"),
                                },
                            }
                        }
                        // Since events buffer is opaque, we cannot suspend our iteration through it
                        // easily. So, we copy the contents to a vector that we can pop from.
                        self.pending_events.extend(self.events.iter().cloned());

                        self.events.clear();
                    }
                    // We have some event to handle. In these cases we
                    // potentially have many events to handle before
                    // reregistering the handle, but pump doesn't assume we are
                    // prepared to handle them all before returning, so we
                    // transition the state from Awaiting to a resource specific
                    // state representing draining all pending events of some
                    // type before reregistering the underlying fd.
                    Some(ev) => match self.fds.remove(ev.token().0) {
                        EventSource::ChildTerminated(mut w) => {
                            match w.handle_event(&mut self.children, &mut self.wait_buffer) {
                                SourceInstruction::Reregister => {
                                    self.reregister(EventSource::ChildTerminated(w));
                                    state = State::DrainingChildTerminated;
                                }
                                SourceInstruction::Deregister => {
                                    self.deregister(EventSource::ChildTerminated(w));
                                }
                            }
                        }
                        EventSource::ReadableChild(child_out) => {
                            state = State::DrainingChildOut(child_out);
                        }
                        #[cfg(feature = "signals")]
                        EventSource::ReceivedSignal(signal_source) => {
                            state = State::DrainingSignals(signal_source);
                        }
                    },
                },
                State::DrainingChildTerminated => match self.wait_buffer.pop() {
                    None => state = State::Awaiting,
                    Some((pid, prog_path, exit_status)) => {
                        let event = Event::ChildTerminated {
                            pid,
                            prog_path: &prog_path,
                            exit_status,
                        };
                        match func(event) {
                            None => state = State::DrainingChildTerminated,
                            Some(r) => {
                                break (State::DrainingChildTerminated, r);
                            }
                        }
                    }
                },
                State::DrainingChildOut(mut child_out) => {
                    let fd = &mut child_out.fd;
                    let buf: &mut String = &mut child_out.buf;
                    match fd.read_line(buf) {
                        Ok(0) => {
                            // The fd was closed; we must deregister the fd and
                            // return to the awaiting state.
                            child_out
                                .fd
                                .get_mut()
                                .deregister(self.poll.registry())
                                .unwrap();
                            let event = Event::FdClosed {
                                pid: child_out.pid,
                                tag: child_out.tag,
                                prog_path: &child_out.prog_path,
                            };
                            match func(event) {
                                None => state = State::Awaiting,
                                Some(r) => {
                                    break (State::Awaiting, r);
                                }
                            }
                        }
                        Ok(_) => {
                            let event = Event::ChildWrote {
                                pid: child_out.pid,
                                tag: child_out.tag,
                                prog_path: &child_out.prog_path,
                                line: buf,
                            };
                            let ores = func(event);
                            buf.clear();
                            match ores {
                                None => state = State::DrainingChildOut(child_out),
                                Some(r) => {
                                    break (State::DrainingChildOut(child_out), r);
                                }
                            }
                        }
                        // maybe we want to break in the future if we start
                        // listening for SIGALRM
                        Err(e) if e.kind() == ErrorKind::Interrupted => {
                            state = State::DrainingChildOut(child_out)
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            self.reregister(EventSource::ReadableChild(child_out));
                            state = State::Awaiting;
                        }
                        Err(e) => panic!("Unexpected error when reading child output: {e}"),
                    }
                }
                #[cfg(feature = "signals")]
                State::DrainingSignals(mut signal_source) => match signal_source.next() {
                    EventStream::Emit(signal) => {
                        let event = Event::SignalReceived { signal };
                        match func(event) {
                            Some(r) => {
                                break (State::DrainingSignals(signal_source), r);
                            }
                            None => state = State::DrainingSignals(signal_source),
                        }
                    }
                    EventStream::Drained(source_instruction) => {
                        let event_source = EventSource::ReceivedSignal(signal_source);
                        match source_instruction {
                            SourceInstruction::Reregister => self.reregister(event_source),
                            SourceInstruction::Deregister => self.deregister(event_source),
                        }
                        state = State::Awaiting;
                    }
                },
            }
        };
        self.state = state;
        event
    }
}

enum EventSource {
    ReadableChild(ChildOut),
    ChildTerminated(ChildTerminationSource),
    #[cfg(feature = "signals")]
    ReceivedSignal(SignalSource),
}

impl Source for EventSource {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        match self {
            EventSource::ReadableChild(x) => x.register(registry, token, interests),
            EventSource::ChildTerminated(x) => x.register(registry, token, interests),
            #[cfg(feature = "signals")]
            EventSource::ReceivedSignal(x) => x.register(registry, token, interests),
        }
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        match self {
            EventSource::ReadableChild(x) => x.reregister(registry, token, interests),
            EventSource::ChildTerminated(x) => x.reregister(registry, token, interests),
            #[cfg(feature = "signals")]
            EventSource::ReceivedSignal(x) => x.reregister(registry, token, interests),
        }
    }

    fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        match self {
            EventSource::ReadableChild(x) => x.deregister(registry),
            EventSource::ChildTerminated(x) => x.deregister(registry),
            #[cfg(feature = "signals")]
            EventSource::ReceivedSignal(x) => x.deregister(registry),
        }
    }
}

#[derive(Debug)]
enum State {
    Awaiting,
    DrainingChildOut(ChildOut),
    DrainingChildTerminated,
    #[cfg(feature = "signals")]
    DrainingSignals(SignalSource),
}

pub struct MuxerChild {
    child: Child,
    prog_path: Rc<PathBuf>,
    exit_status: Rc<Cell<Option<ExitStatus>>>,
}
