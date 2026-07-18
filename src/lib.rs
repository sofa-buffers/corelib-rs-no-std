//! # SofaBuffers (`sofab`) — Rust core library
//!
//! A compact, **streaming**, `#![no_std]` / **heap-free** implementation of the
//! SofaBuffers (Sofab) serialization format, ported from the C `corelib`
//! (`istream.c` / `ostream.c`). It targets very small Cortex-M class devices.
//!
//! The wire format is specified in the [SofaBuffers documentation][docs] and is
//! reproduced byte-for-byte here (the unit tests use the exact test vectors from
//! the C implementation).
//!
//! [docs]: https://github.com/sofa-buffers/documentation
//!
//! ## Design highlights
//!
//! * **`#![no_std]` and `#![forbid(unsafe_code)]`** — no allocator, no `unsafe`.
//!   All state lives in caller-provided buffers / structs.
//! * **Streaming encode** — [`OStream`] writes into a caller buffer and calls a
//!   user [`Flush`] sink whenever the buffer fills, so messages larger than RAM
//!   can be produced incrementally. An initial *offset* leaves room for
//!   lower-layer protocol headers (avoids a copy).
//! * **Streaming decode** — [`IStream`] is a byte-at-a-time state machine. Feed
//!   it arbitrary chunks; it pushes decoded fields to your [`Visitor`]. Large
//!   string/blob payloads are delivered in chunks, so they too may exceed RAM.
//! * **Zero-cost feature gating** — disable `fixlen` / `array` / `sequence` /
//!   `fp64` to drop whole code paths, mirroring the C `SOFAB_DISABLE_*` macros.
//!
//! ## String validity: strict UTF-8 (always on)
//!
//! A `string` field is UTF-8 (MESSAGE_SPEC §8). Because Rust's `str`/`String`
//! is a **Unicode string type**, this port is **always strict** — the
//! `SOFAB_STRICT_UTF8` option (CORELIB_PLAN §6.4) is a **no-op here, pinned ON**,
//! and there is no primitive to expose (only byte-container targets need one):
//!
//! * **Encode is strict by construction.** [`OStream::write_str`] takes `&str`,
//!   already guaranteed valid UTF-8 by the type system, so a `string` field can
//!   never carry invalid bytes — no runtime check is possible or needed.
//!   Arbitrary bytes go in a `blob` via [`OStream::write_blob`].
//! * **Decode strictness lives in generated code.** The corelib delivers a
//!   `string` field's **raw bytes** to [`Visitor::string`] and never builds a
//!   `str`/`String`. Generated code materializes it with `core::str::from_utf8`;
//!   an `Err` becomes the sticky `inv` flag → [`Error::InvalidMsg`] (the
//!   `INVALID` decode outcome). Invalid UTF-8 is **rejected, never replaced**
//!   with `U+FFFD` or truncated (MESSAGE_SPEC §8). Embedded `U+0000` is valid
//!   UTF-8 and round-trips byte-exact. std (`corelib-rs`) and no_std agree here
//!   (generator #80).
//! * **Skipped fields are never validated** — a skipped `string` is a length
//!   jump over bytes the visitor never sees, so no `from_utf8` runs (§6.4).
//!
//! ## Example
//!
//! ```
//! use sofab::{OStream, IStream, Visitor, Id, Unsigned};
//!
//! // --- encode ---
//! let mut buf = [0u8; 32];
//! let used = {
//!     let mut os = OStream::new(&mut buf);
//!     os.write_unsigned(1, 42).unwrap();
//!     os.write_signed(2, -7).unwrap();
//!     os.bytes_used()
//! };
//!
//! // --- decode ---
//! #[derive(Default)]
//! struct Sink { a: u64, b: i64 }
//! impl Visitor for Sink {
//!     fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
//!     fn signed(&mut self, id: Id, v: sofab::Signed) { if id == 2 { self.b = v; } }
//! }
//! let mut sink = Sink::default();
//! let mut is = IStream::new();
//! is.feed(&buf[..used], &mut sink).unwrap();
//! assert_eq!((sink.a, sink.b), (42, -7));
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod istream;
mod ostream;
mod types;
mod varint;

pub use error::{Error, Result};
pub use istream::{IStream, Visitor};
pub use ostream::{Flush, NoFlush, OStream};
pub use types::{Id, Signed, Unsigned, API_VERSION, ID_MAX};

#[cfg(feature = "fixlen")]
pub use types::FixlenType;

#[cfg(feature = "array")]
pub use types::ArrayKind;

#[cfg(feature = "sequence")]
pub use types::MAX_DEPTH;

#[cfg(feature = "array")]
pub use ostream::{SignedElem, UnsignedElem};

/// Compile-time view of how this `sofab` build was configured.
///
/// Each constant reflects the Cargo feature / value-width the **library** was
/// compiled with, so application code can assert that the library supports what
/// it needs — the Rust equivalent of a C `#ifdef` guard. Prefer the
/// [`require!`](crate::require) macro for a ready-made message; these constants
/// are the building blocks (e.g. for `cfg`-free runtime logging of the config).
///
/// ```
/// // Hard-fail the build unless this app is linked against a config it supports.
/// const _: () = assert!(sofab::config::FP64, "this firmware needs sofab fp64");
/// const _: () = assert!(sofab::config::VALUE_BITS >= 64);
/// ```
pub mod config {
    /// Fixed-length fields (`fp32` / `fp64` / string / blob) are compiled in.
    pub const FIXLEN: bool = cfg!(feature = "fixlen");
    /// Array fields are compiled in.
    pub const ARRAY: bool = cfg!(feature = "array");
    /// Nested sequences are compiled in.
    pub const SEQUENCE: bool = cfg!(feature = "sequence");
    /// 64-bit floating point (`fp64`) is compiled in.
    pub const FP64: bool = cfg!(feature = "fp64");
    /// Width of the scalar value type in bits: `64` with the default-on
    /// `value64` feature, or `32` when it is disabled.
    pub const VALUE_BITS: u32 = <crate::Unsigned>::BITS;
}

/// Assert at compile time that this `sofab` build supports what your code needs.
///
/// The Rust equivalent of a C `#ifdef` / `static_assert` guard: each argument is
/// checked against [`config`](crate::config), and a missing capability fails the
/// build with a clear message instead of producing a binary that silently lacks
/// a wire type. Accepts any of `fixlen`, `array`, `sequence`, `fp64`, `value32`,
/// `value64`, separated by commas.
///
/// ```
/// // Compile error unless sofab was built with fp64 + array support and 64-bit values.
/// sofab::require!(fp64, array, value64);
/// ```
///
/// A build with, say, `default-features = false` (no `fp64`) would stop here:
///
/// ```compile_fail
/// # // doctest is built with default features, so force the failure explicitly:
/// const _: () = assert!(!sofab::config::FP64, "see require! docs");
/// ```
#[macro_export]
macro_rules! require {
    (fixlen) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::FIXLEN,
            "sofab: this application requires the `fixlen` feature, but it is disabled"
        );
    };
    (array) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::ARRAY,
            "sofab: this application requires the `array` feature, but it is disabled"
        );
    };
    (sequence) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::SEQUENCE,
            "sofab: this application requires the `sequence` feature, but it is disabled"
        );
    };
    (fp64) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::FP64,
            "sofab: this application requires the `fp64` feature, but it is disabled"
        );
    };
    (value32) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::VALUE_BITS == 32,
            "sofab: this application requires the 32-bit value width (disable the default `value64` feature)"
        );
    };
    (value64) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::VALUE_BITS == 64,
            "sofab: this application requires the 64-bit value width (the default `value64` feature is disabled)"
        );
    };
    ($($cap:ident),+ $(,)?) => {
        $( $crate::require!($cap); )+
    };
}
