use std::{collections::BTreeMap, io, path::PathBuf, process::ExitStatus, rc::Rc};

use crate::muxer::source::SourceInstruction;
use crate::muxer::MuxerChild;
use crate::Pid;
use mio::event::Source;
use signal_hook_mio::v0_8::Signals;

pub struct ChildTerminationSource {
    signals: Signals,
}

impl Source for ChildTerminationSource {
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

impl ChildTerminationSource {
    pub fn new() -> io::Result<Self> {
        let signals = Signals::new([libc::SIGCHLD])?;
        let res = Self { signals };
        Ok(res)
    }

    pub fn handle_event(
        &mut self,
        children: &mut BTreeMap<Pid, MuxerChild>,
        buffer: &mut Vec<(Pid, Rc<PathBuf>, ExitStatus)>,
    ) -> SourceInstruction {
        if self.signals.pending().last().is_some() {
            for (pid, muxer_child) in children.iter_mut() {
                let child = &mut muxer_child.child;
                if let Some(exit_status) = child.try_wait().unwrap() {
                    muxer_child.exit_status.replace(Some(exit_status));
                    let awaited_child = (*pid, muxer_child.prog_path.clone(), exit_status);
                    buffer.push(awaited_child);
                }
            }

            for (pid, _, _) in buffer.iter() {
                let _ = children.remove(pid);
            }
        }

        SourceInstruction::Reregister
    }
}
