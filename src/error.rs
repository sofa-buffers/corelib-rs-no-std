//! Error and result types.
//!
//! Mirrors the C `sofab_ret_t` status codes (minus `OK`, which Rust models as
//! `Ok(())`).

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Invalid caller argument (e.g. a field id greater than [`crate::ID_MAX`]).
    ///
    /// Corresponds to `SOFAB_RET_E_ARGUMENT`.
    Argument,

    /// Invalid API usage (e.g. a decoded value does not fit the requested type).
    ///
    /// Corresponds to `SOFAB_RET_E_USAGE`. Reserved for §6.3 baseline parity
    /// with the other ports: the push-by-value [`Visitor`](crate::Visitor)
    /// decode model has no read-type-mismatch path, so this port never
    /// constructs it today, but it is kept so the error set matches the
    /// cross-language baseline.
    Usage,

    /// The output buffer is full and no [`crate::Flush`] sink is available.
    ///
    /// Corresponds to `SOFAB_RET_E_BUFFER_FULL`.
    BufferFull,

    /// The input bytes are not a valid Sofab message (varint overflow, bad type
    /// tag, oversized length/count, nesting past `MAX_DEPTH`, dangling sequence
    /// end, …).
    ///
    /// Malformed **regardless of what follows** — a terminal `INVALID` outcome
    /// (`MESSAGE_SPEC.md` §7). Corresponds to `SOFAB_RET_E_INVALID_MSG`.
    InvalidMsg,

    /// The consumed bytes end **inside** a field — an unterminated varint (the
    /// `0x80` continuation flag was set but the stream stopped), a fixlen /
    /// string / blob payload shorter than its declared length, or a nested
    /// sequence that is not yet closed.
    ///
    /// This is the `INCOMPLETE` outcome of `MESSAGE_SPEC.md` §7 and is
    /// **explicitly not an error**: it is a first-class, distinct result that a
    /// decoder MUST report rather than fold into either neighbour. Feeding more
    /// bytes may complete the field (turning the next outcome into `Ok(())`) or
    /// reveal it as malformed ([`Error::InvalidMsg`]). The decoder never decides
    /// on the caller's behalf that this prefix is "truncated" — end-of-input is
    /// the caller's own framing decision (§7.1), so there is deliberately no
    /// `finish`/`finalize` step that would reclassify it.
    ///
    /// It rides the `Result` channel (as `Err`) purely so `feed` keeps a single
    /// return type; a streaming caller reads it as "feed me the next chunk", a
    /// one-shot / framed caller as "truncated at my layer".
    Incomplete,
}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
