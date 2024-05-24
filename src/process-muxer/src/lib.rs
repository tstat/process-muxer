use std::{
    io::{self, stderr, stdout, LineWriter, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use console::Style;
pub use process_muxer_core::{ChildInfo, Event, FdTag, Pid, Signal};
use regex::Regex;

pub trait MuxerHook {
    fn before_event<'a>(&mut self, event: &Event<'a>);
    fn before_spawn(&mut self, command: &Command);
}

pub struct Muxer {
    inner: process_muxer_core::Muxer,
    hooks: Vec<Box<dyn MuxerHook>>,
}

impl Muxer {
    pub fn new() -> io::Result<Self> {
        let res = Muxer {
            inner: process_muxer_core::Muxer::new()?,
            hooks: Vec::new(),
        };
        Ok(res)
    }

    pub fn add_hook<T: MuxerHook + 'static>(&mut self, hook: T) {
        self.hooks.push(Box::new(hook));
    }

    pub fn pump<R, F>(&mut self, mut func: F) -> R
    where
        F: FnMut(Event) -> Option<R>,
    {
        self.inner.pump(|ev| {
            for hook in self.hooks.iter_mut() {
                hook.before_event(&ev);
            }
            func(ev)
        })
    }

    /// Send SIGTERM to all children and wait for them to exit.
    pub fn cleanup(&mut self) -> io::Result<()> {
        let mut child_count = 0;
        for pid in self.inner.pids() {
            child_count += 1;
            unsafe { libc::kill(pid.inner as i32, libc::SIGTERM) };
        }

        if child_count > 0 {
            self.pump(|ev| {
                if let Event::ChildTerminated { .. } = ev {
                    child_count -= 1;
                    if child_count == 0 {
                        return Some(());
                    }
                }
                None
            });
        }
        Ok(())
    }

    /// Create a new child process that inherits stdin, stdout, and stderr and
    /// is a member of the same process group.
    pub fn control(&mut self, cmd: Command) -> io::Result<ChildInfo> {
        let child = self.spawn(cmd)?;
        Ok(child)
    }

    pub fn forward(&mut self, mut cmd: Command) -> io::Result<ChildInfo> {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.process_group(0);
        let child = self.spawn(cmd)?;
        Ok(child)
    }

    fn spawn(&mut self, cmd: Command) -> io::Result<ChildInfo> {
        for hook in self.hooks.iter_mut() {
            hook.before_spawn(&cmd);
        }

        self.inner.spawn(cmd)
    }

    pub fn wait_for_signal(&mut self) -> Signal {
        use Event::*;
        self.pump(|ev| match ev {
            SignalReceived { signal } => Some(signal),
            _ => None,
        })
    }

    pub fn wait_for_match(&mut self, child_info: &ChildInfo, re: Regex) -> Result<()> {
        use Event::*;
        if let Some(exit_status) = child_info.exit_status() {
            return Err(Error::UnexpectedChildTermination {
                pid: child_info.pid,
                prog_path: PathBuf::from(child_info.program()),
                exit_status,
            });
        }
        self.pump(|ev| match ev {
            ChildTerminated {
                pid,
                exit_status,
                prog_path,
            } if pid == child_info.pid => Some(Err(Error::UnexpectedChildTermination {
                pid,
                prog_path: PathBuf::from(prog_path),
                exit_status,
            })),
            ChildWrote { pid, line, .. } if pid == child_info.pid && re.is_match(line) => {
                Some(Ok(()))
            }
            // todo: watch for stdout and stderr closing. We need to know the
            // initial state though.
            FdClosed { .. } => None,
            SignalReceived { signal } => Some(Err(Error::from(signal))),
            _ => None,
        })
    }

    pub fn wait(&mut self, child_info: &ChildInfo) -> Result<ExitStatus> {
        use Event::*;
        if let Some(exit_status) = child_info.exit_status() {
            return Ok(exit_status);
        }
        self.pump(|ev| match ev {
            ChildTerminated {
                pid, exit_status, ..
            } if pid == child_info.pid => Some(Ok(exit_status)),
            SignalReceived { signal } => Some(Err(Error::from(signal))),
            _ => None,
        })
    }
}

#[derive(Debug)]
pub enum Error {
    UnexpectedChildTermination {
        pid: Pid,
        prog_path: PathBuf,
        exit_status: ExitStatus,
    },
    UnexpectedSignal {
        signal: Signal,
    },
}

pub type Result<A> = std::result::Result<A, Error>;

impl From<Signal> for Error {
    fn from(signal: Signal) -> Self {
        Error::UnexpectedSignal { signal }
    }
}

pub struct PrintInfo<Stdout: Write, Stderr: Write> {
    pub stdout: Stdout,
    pub stderr: Stderr,
    pub info_style: Style,
    pub stdout_style: Style,
    pub stderr_style: Style,
}

impl PrintInfo<LineWriter<io::Stdout>, LineWriter<io::Stderr>> {
    pub fn new() -> Self {
        Self {
            stdout: LineWriter::new(stdout()),
            stderr: LineWriter::new(stderr()),
            info_style: Style::new().green(),
            stdout_style: Style::new().white(),
            stderr_style: Style::new().white(),
        }
    }
}

impl<Stdout: Write, Stderr: Write> MuxerHook for PrintInfo<Stdout, Stderr> {
    fn before_event<'a>(&mut self, ev: &Event<'a>) {
        match ev {
            Event::ChildTerminated {
                prog_path,
                exit_status,
                ..
            } => {
                writeln!(
                    &mut self.stdout,
                    "{}{} {} {}{}",
                    self.info_style.apply_to("["),
                    self.info_style.apply_to(prog_path.display()),
                    self.info_style.apply_to("terminated with"),
                    self.info_style.apply_to(exit_status),
                    self.info_style.apply_to("]"),
                )
                .unwrap();
            }
            Event::ChildWrote {
                prog_path,
                tag,
                line,
                ..
            } => {
                let forward_style = match tag {
                    FdTag::Stdout => &self.stdout_style,
                    FdTag::Stderr => &self.stderr_style,
                };
                let output: &mut dyn Write = match tag {
                    FdTag::Stdout => &mut self.stdout,
                    FdTag::Stderr => &mut self.stderr,
                };
                write!(
                    output,
                    "{}{}{} {}",
                    self.info_style.apply_to("["),
                    self.info_style.apply_to(&prog_path.display()),
                    self.info_style.apply_to("]"),
                    forward_style.apply_to(line),
                )
                .unwrap();
            }
            Event::FdClosed { prog_path, tag, .. } => {
                let handle: &str = match tag {
                    FdTag::Stderr => "stderr",
                    FdTag::Stdout => "stdout",
                };
                writeln!(
                    &mut self.stdout,
                    "{}{} {} {}{}",
                    self.info_style.apply_to("["),
                    self.info_style.apply_to(prog_path.display()),
                    self.info_style.apply_to("closed"),
                    self.info_style.apply_to(handle),
                    self.info_style.apply_to("]"),
                )
                .unwrap();
            }
            Event::SignalReceived { ref signal } => {
                let signal = match signal {
                    Signal::Hangup => "hangup (SIGHUP)",
                    Signal::Interrupt => "interrupt (SIGINT)",
                    Signal::Terminate => "terminate (SIGTERM)",
                };
                writeln!(
                    &mut self.stdout,
                    "{}{} {}{}",
                    self.info_style.apply_to("["),
                    self.info_style.apply_to("Received signal: "),
                    self.info_style.apply_to(signal),
                    self.info_style.apply_to("]"),
                )
                .unwrap();
            }
        }
    }

    fn before_spawn(&mut self, cmd: &Command) {
        let prog_path: &Path = Path::new(cmd.get_program());
        writeln!(
            &mut self.stdout,
            "{} {}{}",
            self.info_style.apply_to("[Running"),
            self.info_style.apply_to(prog_path.display()),
            self.info_style.apply_to("]")
        )
        .unwrap();
    }
}
