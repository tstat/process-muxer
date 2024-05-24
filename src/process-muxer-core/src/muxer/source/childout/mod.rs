use std::{
    io::{self, BufReader},
    path::PathBuf,
    process::{ChildStderr, ChildStdout},
    rc::Rc,
};

use mio::{event::Source, unix::pipe, Interest, Token};

use crate::Pid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdTag {
    Stderr,
    Stdout,
}

#[derive(Debug)]
pub struct ChildOut {
    pub pid: Pid,
    pub prog_path: Rc<PathBuf>,
    pub tag: FdTag,
    pub buf: String,
    pub fd: BufReader<pipe::Receiver>,
}

impl ChildOut {
    pub(crate) fn from_pipe<T: Into<pipe::Receiver> + TaggedFd>(
        value: T,
        pid: Pid,
        prog_path: Rc<PathBuf>,
    ) -> Self {
        let pipe: pipe::Receiver = value.into();
        pipe.set_nonblocking(true)
            .expect("setting nonblocking to succeed");
        ChildOut {
            pid,
            prog_path,
            tag: T::fdtag(),
            buf: String::with_capacity(1024),
            fd: BufReader::with_capacity(8192, pipe),
        }
    }
}

impl Source for ChildOut {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        self.fd.get_mut().register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        self.fd.get_mut().reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        self.fd.get_mut().deregister(registry)
    }
}

pub(crate) trait TaggedFd {
    fn fdtag() -> FdTag;
}

impl TaggedFd for ChildStdout {
    fn fdtag() -> FdTag {
        FdTag::Stdout
    }
}

impl TaggedFd for ChildStderr {
    fn fdtag() -> FdTag {
        FdTag::Stderr
    }
}
