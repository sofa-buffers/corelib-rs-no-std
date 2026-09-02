//! Error and result types.
//!
//! Mirrors the C `sofab_ret_t` status codes (minus `OK`, which Rust models as
//! the `Ok` arm) and the std port's (`corelib-rs`) [`Error`], so code moves
//! between the two Rust crates unchanged.
//!
//! **`INCOMPLETE` is not here, by design.** CORELIB_PLAN §5.2.1 calls it a
//! first-class decode *outcome* and not an error, and §6.3 keeps the per-`feed`
//! result ("the three-valued outcome … *not* a code from this table") apart from
//! this table. It is therefore carried in the success arm, as
//! [`crate::Status::Incomplete`]; what remains here is what genuinely failed, so
//! `?` propagates real errors instead of the commonest normal case.

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Invalid caller argument (e.g. a field id greater than [`crate::ID_MAX`]).
    ///
    /// Corresponds to `SOFAB_RET_E_ARGUMENT`.
    Argument,

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
    ///
    /// This is distinct from [`crate::Status::Incomplete`]: a truncated message
    /// is *not* malformed, it is merely unfinished — and it is not an error at
    /// all, so it never reaches this enum.
    InvalidMsg,

    /// A receiver-configured decode limit was exceeded: a field the **schema**
    /// leaves unbounded carried more elements or more bytes than this receiver
    /// is willing to accept (`max_dyn_array_count`, `max_dyn_string_len`,
    /// `max_dyn_blob_len` — CORELIB_PLAN §6.2.1).
    ///
    /// **Policy, not malformation**, and therefore deliberately distinct from
    /// [`Error::InvalidMsg`]: the same bytes are well-formed and decode
    /// perfectly for a receiver configured with a higher limit, so a peer that
    /// reads a limit rejection as a wire divergence has drawn the wrong
    /// conclusion (§6.3). It is terminal — the field is **rejected, never
    /// clamped or truncated** — and it is never raised for a field the schema
    /// already bounds, where an over-long value is `INVALID` instead.
    ///
    /// This corelib **enforces no limit of its own**: it has no configuration,
    /// invents no default, and this variant is never returned from
    /// [`crate::IStream::feed`]. What the codec contributes is the report the
    /// decision is made on — the element count on `Visitor::array_begin`,
    /// the payload length on `Visitor::fixlen_begin`, the element id
    /// inside a sequence array — all of them delivered at the count/length word,
    /// before anything is materialized. The generated visitor holds the numbers,
    /// checks them there, and reports a breach to *its* caller under this name,
    /// which exists here so that every port of the family spells the category
    /// the same way (`SOFAB_RET_E_LIMIT_EXCEEDED` in C, `Error::LimitExceeded`
    /// in `corelib-rs`).
    ///
    /// On this profile it is rare by construction: a heap-free receiver sizes
    /// every destination from the schema, and a schema-bounded field is capped
    /// by its own bound (§6.2.1), not by a receiver limit.
    LimitExceeded,
}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
