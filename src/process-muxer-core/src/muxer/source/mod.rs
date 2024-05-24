pub(crate) mod childout;
#[cfg(feature = "signals")]
pub(crate) mod signal;
pub(crate) mod termination;

pub enum EventStream<T> {
    Emit(T),
    Drained(SourceInstruction),
}

pub enum SourceInstruction {
    Reregister,
    #[allow(dead_code)]
    // once pidfd support lands in stable this will be used.
    // https://github.com/rust-lang/rust/issues/82971
    Deregister,
}
