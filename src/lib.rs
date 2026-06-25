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

#[cfg(feature = "array")]
pub use ostream::{SignedElem, UnsignedElem};
