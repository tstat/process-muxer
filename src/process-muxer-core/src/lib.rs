pub(crate) mod muxer;
#[cfg(feature = "signals")]
pub use muxer::source::signal::Signal;
pub use muxer::{ChildInfo, Event, FdTag, Muxer, Pid};
