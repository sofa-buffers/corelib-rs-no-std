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
    /// Corresponds to `SOFAB_RET_E_INVALID_MSG`.
    InvalidMsg,
}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
