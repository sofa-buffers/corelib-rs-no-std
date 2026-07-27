//! Streaming output stream encoder (port of `ostream.c`).
//!
//! [`OStream`] writes Sofab fields into a caller-owned byte buffer. When the
//! buffer fills it hands the bytes to an optional [`Flush`] sink and resumes at
//! the start of the buffer, so messages larger than the buffer (or larger than
//! RAM) can be streamed out. With no sink, a full buffer yields
//! [`Error::BufferFull`].

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::zigzag_encode;
use crate::{Id, Signed, Unsigned};

/// Sink that receives buffered bytes when the output buffer is flushed.
///
/// Any `FnMut(&[u8])` closure implements this trait, so callbacks can be passed
/// directly. Implement it manually to avoid a closure capture on bare-metal.
pub trait Flush {
    /// Whether this sink actually drains the buffer. `true` for a real sink (a
    /// full buffer flushes and writing resumes); [`NoFlush`] overrides it to
    /// `false` so a full buffer is [`Error::BufferFull`] instead. It is a
    /// compile-time constant, so a [`NoFlush`] encoder monomorphizes the
    /// flush branch out of `push_byte` entirely (no field, no dead code).
    const SINKS: bool = true;

    /// Consume `data` (e.g. push to a transport or storage). Called with the
    /// bytes accumulated since the last flush.
    fn flush(&mut self, data: &[u8]);
}

impl<T: FnMut(&[u8])> Flush for T {
    #[inline]
    fn flush(&mut self, data: &[u8]) {
        self(data)
    }
}

/// A [`Flush`] sink that does nothing. Used as the default when the stream is
/// constructed without a sink; a full buffer then returns [`Error::BufferFull`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFlush;

impl Flush for NoFlush {
    const SINKS: bool = false;
    #[inline]
    fn flush(&mut self, _data: &[u8]) {}
}

/// How many nested sequence headers can be held back at once (see
/// [`OStream::write_sequence_begin_lazy`]). A run deeper than this is framed
/// eagerly: still valid, just not canonical — an all-default sequence nested
/// deeper keeps its empty frame, which a decoder accepts and normalizes away
/// (MESSAGE_SPEC §2). Deliberately far below the format's [`MAX_DEPTH`] ceiling:
/// the array costs `4 * LAZY_SEQ_DEPTH` bytes of encoder state, and a heap-free
/// target pays that in RAM — measured on Cortex-M0, the `OStream` grows from
/// 16 B to 52 B at 8.
///
/// It is **fixed at 8 for every build of this crate**: there is no Cargo feature,
/// no `cfg` and no environment variable that changes it, so a target that cannot
/// spare the 36 B has to edit this constant in a patched or vendored copy of the
/// crate. A schema nesting deeper than the window still encodes correctly either
/// way; it just keeps the empty frame of the sequences beyond it.
///
/// This bound is the **heap-free profile allowance** of CORELIB_PLAN §6 ("How
/// deep the hold-back reaches"): an implementation that can allocate MUST hold
/// back to the full [`MAX_DEPTH`] and is canonical at every depth; a heap-free
/// one MAY bound the run, and **MUST document the bound**, because two encoders
/// that disagree about it disagree about bytes — not about validity. That
/// documentation, with the measured RAM cost next to it, is the README's
/// "Sequence framing" section; keep the two in sync when changing this value.
#[cfg(feature = "sequence")]
pub const LAZY_SEQ_DEPTH: usize = 8;

/// Streaming Sofab encoder writing into a caller-provided buffer.
pub struct OStream<'a, F: Flush = NoFlush> {
    buffer: &'a mut [u8],
    offset: usize,
    /// The flush sink. Whether it drains a full buffer or errors is decided at
    /// compile time by [`Flush::SINKS`]; [`NoFlush`] is zero-sized and folds the
    /// flush branch away, so a no-sink encoder carries no runtime sink state.
    flush: F,
    /// Currently-open nested-sequence depth, capped at [`MAX_DEPTH`].
    #[cfg(feature = "sequence")]
    depth: u32,
    /// Ids of the innermost open sequences whose header has not been written yet
    /// (MESSAGE_SPEC §2 lazy framing). Always a contiguous suffix of the open
    /// sequences: writing any field commits the whole run at once.
    #[cfg(feature = "sequence")]
    pending: [Id; LAZY_SEQ_DEPTH],
    /// Number of valid entries in [`Self::pending`].
    #[cfg(feature = "sequence")]
    npending: usize,
}

impl<'a> OStream<'a, NoFlush> {
    /// Create an encoder over `buffer` with no flush sink. Writing past the end
    /// of the buffer returns [`Error::BufferFull`].
    #[inline]
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self::with_offset(buffer, 0)
    }

    /// Like [`OStream::new`] but begin writing at `offset` bytes into the
    /// buffer, reserving space for a lower-layer protocol header.
    #[inline]
    pub fn with_offset(buffer: &'a mut [u8], offset: usize) -> Self {
        OStream {
            buffer,
            offset,
            flush: NoFlush,
            #[cfg(feature = "sequence")]
            depth: 0,
            #[cfg(feature = "sequence")]
            pending: [0; LAZY_SEQ_DEPTH],
            #[cfg(feature = "sequence")]
            npending: 0,
        }
    }
}

impl<'a, F: Flush> OStream<'a, F> {
    /// Create an encoder with a flush `sink`, starting at `offset`. When the
    /// buffer fills, the accumulated bytes are passed to `sink` and writing
    /// resumes at the start of the buffer.
    #[inline]
    pub fn with_flush(buffer: &'a mut [u8], offset: usize, sink: F) -> Self {
        OStream {
            buffer,
            offset,
            flush: sink,
            #[cfg(feature = "sequence")]
            depth: 0,
            #[cfg(feature = "sequence")]
            pending: [0; LAZY_SEQ_DEPTH],
            #[cfg(feature = "sequence")]
            npending: 0,
        }
    }

    /// Number of bytes written to the active buffer since the last flush.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.offset
    }

    /// Flush any pending bytes to the sink (if one is set) and report how many
    /// bytes were pending. With no sink the buffer is left intact.
    pub fn flush(&mut self) -> usize {
        let used = self.offset;
        // `F::SINKS` is a compile-time constant: for a `NoFlush` encoder the
        // whole body folds away and this is just `self.offset`.
        if used > 0 && F::SINKS {
            self.flush.flush(&self.buffer[..used]);
            self.offset = 0;
        }
        used
    }

    /// Replace the active buffer (typically called from within a flush sink),
    /// resuming writes at `offset` in the new buffer.
    #[inline]
    pub fn buffer_set(&mut self, buffer: &'a mut [u8], offset: usize) {
        self.buffer = buffer;
        self.offset = offset;
    }

    // --- primitives ---------------------------------------------------------

    fn push_byte(&mut self, b: u8) -> Result<()> {
        if self.offset >= self.buffer.len() {
            // `F::SINKS` is a compile-time constant. A `NoFlush` encoder folds
            // this to an unconditional `BufferFull` (no flush field access, no
            // dead sink call); a real sink keeps the flush-and-resume path.
            if !F::SINKS {
                return Err(Error::BufferFull);
            }
            // `min` proves the slice end in-bounds (offset == len here in
            // normal use), so no panicking bounds check is emitted.
            let used = self.offset.min(self.buffer.len());
            self.flush.flush(&self.buffer[..used]);
            self.offset = 0;
        }
        // `get_mut` folds the buffer-full guard and the store into one checked
        // access: `None` only for a zero-length buffer, reported as `BufferFull`
        // instead of panicking.
        match self.buffer.get_mut(self.offset) {
            Some(slot) => {
                *slot = b;
                self.offset += 1;
                Ok(())
            }
            None => Err(Error::BufferFull),
        }
    }

    #[cfg_attr(not(feature = "fixlen"), allow(dead_code))]
    fn push_raw(&mut self, data: &[u8]) -> Result<()> {
        for &b in data {
            self.push_byte(b)?;
        }
        Ok(())
    }

    fn write_varint(&mut self, mut value: Unsigned) -> Result<()> {
        loop {
            let mut b = (value as u8) & 0x7F;
            value >>= 7;
            if value != 0 {
                b |= 0x80;
            }
            self.push_byte(b)?;
            if value == 0 {
                return Ok(());
            }
        }
    }

    fn write_id_type(&mut self, id: Id, wire_type: u8) -> Result<()> {
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        // The single choke point every field write passes through, so also where a
        // held-back sequence run is committed: the field about to be written is
        // content, which means every enclosing sequence must be framed after all.
        //
        // Unconditional on purpose — no wire-type exemption. Dropping a sequence
        // is the one case that must *not* commit, and it is expressed by not
        // reaching this function at all: `write_sequence_end` returns before the
        // call, and `write_sequence_begin_lazy` never routes through it. Anything
        // that does reach here is content, including the `SEQUENCE_END` of a kept
        // frame (whose run `write_sequence_end_keep` therefore no longer has to
        // commit itself). Exempting a wire type here would move completeness of
        // the choke point onto its callers' discipline.
        #[cfg(feature = "sequence")]
        if self.npending != 0 {
            self.commit_pending()?;
        }
        self.write_varint(((id as Unsigned) << 3) | wire_type as Unsigned)
    }

    /// Write out the held-back sequence headers, outermost first.
    ///
    /// Cold: it runs at most once per non-default sequence, never per field.
    ///
    /// Only the headers that actually **reached the buffer** are dropped from the
    /// run. If the buffer fills mid-run with no sink to drain it, the sequences
    /// not written yet stay pending — still the innermost contiguous suffix of the
    /// open sequences, so the type's invariant holds and a caller that installs a
    /// bigger buffer ([`OStream::buffer_set`]) resumes exactly where the cut fell.
    /// Clearing the run up front instead would destroy those ids: their
    /// `SEQUENCE_START` markers would never be emitted while their `SEQUENCE_END`s
    /// still are, i.e. a structurally broken stream rather than a truncated one.
    ///
    /// The recovery this buys is real but **bounded**: it only reconstructs the
    /// stream when the cut lands on a *header-varint boundary*. No writer in this
    /// encoder is atomic on failure — a multi-byte header (id > 15) can be cut in
    /// half by the same buffer end, and its leading bytes are already in the
    /// buffer while the id is still pending, so resuming re-emits it whole. As
    /// everywhere else in this encoder, `BufferFull` without a sink leaves a
    /// partial message behind; what is fixed here is that it no longer leaves an
    /// *inconsistent* one on the boundary case.
    #[cfg(feature = "sequence")]
    #[cold]
    #[inline(never)]
    fn commit_pending(&mut self) -> Result<()> {
        let mut written = 0;
        let mut result = Ok(());
        for i in 0..self.npending {
            // `get` rather than `self.pending[i]`: `i < npending <= LAZY_SEQ_DEPTH`
            // holds by construction, but the indexing form still emits a
            // `core::panicking::panic_bounds_check` path that the linker then
            // keeps in the image. The whole codec is meant to link without
            // `core::panicking` (README "Footprint"), so prove the access
            // in-bounds instead of asserting it.
            let id = match self.pending.get(i) {
                Some(&id) => id,
                None => break,
            };
            if let Err(e) =
                self.write_varint(((id as Unsigned) << 3) | T_SEQUENCE_START as Unsigned)
            {
                result = Err(e);
                break;
            }
            written += 1;
        }
        self.drop_front(written);
        result
    }

    /// Drop the outermost `k` entries of the pending run, keeping the rest as the
    /// innermost suffix. Panic-free by the same rule as [`Self::commit_pending`]:
    /// `copy_within` carries a range assert, so the shift is spelled out with
    /// `get`/`get_mut` instead.
    #[cfg(feature = "sequence")]
    fn drop_front(&mut self, k: usize) {
        if k >= self.npending {
            self.npending = 0;
            return;
        }
        let remaining = self.npending - k;
        for i in 0..remaining {
            let id = match self.pending.get(i + k) {
                Some(&id) => id,
                None => break,
            };
            match self.pending.get_mut(i) {
                Some(slot) => *slot = id,
                None => break,
            }
        }
        self.npending = remaining;
    }

    // --- scalar writers -----------------------------------------------------

    /// Write an unsigned-integer field.
    pub fn write_unsigned(&mut self, id: Id, value: Unsigned) -> Result<()> {
        self.write_id_type(id, T_VARINT_UNSIGNED)?;
        self.write_varint(value)
    }

    /// Write a signed-integer field (ZigZag + varint).
    pub fn write_signed(&mut self, id: Id, value: Signed) -> Result<()> {
        self.write_id_type(id, T_VARINT_SIGNED)?;
        self.write_varint(zigzag_encode(value))
    }

    /// Write a boolean as an unsigned `0` / `1`.
    #[inline]
    pub fn write_boolean(&mut self, id: Id, value: bool) -> Result<()> {
        self.write_unsigned(id, value as Unsigned)
    }

    // --- fixed-length writers ----------------------------------------------

    /// Write a fixed-length field: header, `(len << 3) | subtype` varint, then
    /// the raw `data` bytes (already in wire/little-endian order for floats).
    #[cfg(feature = "fixlen")]
    pub fn write_fixlen(&mut self, id: Id, data: &[u8], subtype: FixlenType) -> Result<()> {
        self.write_id_type(id, T_FIXLEN)?;
        self.write_varint(((data.len() as Unsigned) << 3) | subtype as Unsigned)?;
        self.push_raw(data)
    }

    /// Write a 32-bit float field.
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_fp32(&mut self, id: Id, value: f32) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp32)
    }

    /// Write a 64-bit float field.
    #[cfg(feature = "fp64")]
    #[inline]
    pub fn write_fp64(&mut self, id: Id, value: f64) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp64)
    }

    /// Write a string field (raw UTF-8 bytes, no NUL on the wire).
    ///
    /// The input is `&str`, so it is **already valid UTF-8** by the type system
    /// — encode is strict by construction and can never emit non-UTF-8 bytes
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). For arbitrary bytes use
    /// [`OStream::write_blob`]. Embedded `U+0000` is permitted and written
    /// verbatim (the wire is length-framed, no NUL terminator).
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_str(&mut self, id: Id, text: &str) -> Result<()> {
        self.write_fixlen(id, text.as_bytes(), FixlenType::Str)
    }

    /// Write a binary blob field.
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_blob(&mut self, id: Id, data: &[u8]) -> Result<()> {
        self.write_fixlen(id, data, FixlenType::Blob)
    }

    // --- array writers ------------------------------------------------------

    /// Write an array of unsigned integers (`u8`/`u16`/`u32`/`u64` elements).
    ///
    /// Element width is fixed by the type at compile time, so the invalid
    /// element-size error from the C API is impossible here.
    #[cfg(feature = "array")]
    pub fn write_array_unsigned<T: UnsignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        self.write_id_type(id, T_VARINTARRAY_UNSIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(e.widen())?;
        }
        Ok(())
    }

    /// Write an array of signed integers (`i8`/`i16`/`i32`/`i64` elements).
    #[cfg(feature = "array")]
    pub fn write_array_signed<T: SignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        self.write_id_type(id, T_VARINTARRAY_SIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(zigzag_encode(e.widen()))?;
        }
        Ok(())
    }

    /// Write an array of 32-bit floats.
    ///
    /// The `fixlen_word` is **always** present — even for a zero-count array —
    /// so an empty `fp32` array stays distinguishable from an empty `fp64` one
    /// (§4.8): the field is `[ header ][ count ][ fixlen_word ][ payload… ]`,
    /// where an empty array simply has no payload.
    #[cfg(all(feature = "array", feature = "fixlen"))]
    pub fn write_array_fp32(&mut self, id: Id, data: &[f32]) -> Result<()> {
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((4 << 3) | FixlenType::Fp32 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    /// Write an array of 64-bit floats.
    ///
    /// The `fixlen_word` is **always** present — even for a zero-count array —
    /// so an empty `fp64` array stays distinguishable from an empty `fp32` one
    /// (§4.8): the field is `[ header ][ count ][ fixlen_word ][ payload… ]`,
    /// where an empty array simply has no payload.
    #[cfg(all(feature = "array", feature = "fp64"))]
    pub fn write_array_fp64(&mut self, id: Id, data: &[f64]) -> Result<()> {
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((8 << 3) | FixlenType::Fp64 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    // --- sequence writers ---------------------------------------------------

    /// Open a nested sequence whose header is **held back** until the sequence
    /// turns out to have content.
    ///
    /// MESSAGE_SPEC §2 omits a sequence-typed field whose value equals its declared
    /// default, and "not one child was written" is exactly that condition —
    /// evaluated per child field, recursively, for free. A sequence closed with
    /// nothing in it therefore emits **nothing** instead of a two-byte empty frame,
    /// and an all-default message becomes the empty byte string.
    ///
    /// The predicate is never a byte image of the object, so struct padding cannot
    /// influence it and a non-zero nested default is handled by the caller's
    /// ordinary per-field test.
    ///
    /// This is the only way to open a sequence. How it closes decides whether a
    /// contentless one survives: [`OStream::write_sequence_end`] drops it,
    /// [`OStream::write_sequence_end_keep`] forces the frame out.
    ///
    /// Costs no output bytes and no allocation; the held-back ids live in a
    /// [`LAZY_SEQ_DEPTH`]-slot array inside the encoder. That window is the one
    /// place this heap-free port is not canonical: opening a sequence while the
    /// window is full commits the pending run and frames this one **eagerly**, so
    /// an all-default sequence nested deeper than [`LAZY_SEQ_DEPTH`] keeps the
    /// empty frame §2 would have omitted — well-formed, decodes to the same
    /// value, documented in the README (CORELIB_PLAN §6).
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_begin_lazy(&mut self, id: Id) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Argument);
        }
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        // `get_mut` is the panic-free spelling of `self.npending < LAZY_SEQ_DEPTH`
        // followed by an index: `None` *is* the window-full case.
        if let Some(slot) = self.pending.get_mut(self.npending) {
            *slot = id;
            self.npending += 1;
        } else {
            // Deeper than the hold-back window: commit the run and frame eagerly,
            // which keeps the suffix invariant above. Valid, just not canonical if
            // this sequence turns out to be all-default (CORELIB_PLAN §6, the
            // heap-free allowance — see [`LAZY_SEQ_DEPTH`]).
            self.commit_pending()?;
            self.write_varint(((id as Unsigned) << 3) | T_SEQUENCE_START as Unsigned)?;
        }
        self.depth += 1;
        Ok(())
    }

    /// Close the most recently opened nested sequence, letting it **vanish** if it
    /// received no content.
    ///
    /// Use it wherever absence encodes the same value as an empty frame: a
    /// `struct`/`union` field, and an array field whose declared `default` is the
    /// empty collection (MESSAGE_SPEC §2). Where the frame must be visible, close
    /// with [`OStream::write_sequence_end_keep`] instead.
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_end(&mut self) -> Result<()> {
        if self.npending != 0 {
            // The innermost open sequence is the last held-back one: drop it.
            self.npending -= 1;
            self.depth = self.depth.saturating_sub(1);
            return Ok(());
        }
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }

    /// Close the most recently opened nested sequence, **keeping** its frame even
    /// when it received no content.
    ///
    /// Behaves like a write: it first emits any held-back headers — this frame's
    /// and every enclosing one's — and then the end marker, so an empty sequence
    /// reaches the wire as `begin` + `end`.
    ///
    /// Required wherever the frame carries information beyond its contents:
    /// - a **wrapper-array element** (`struct`/`union`/nested row): element
    ///   presence is what carries a dynamic array's length — *highest present id +
    ///   1* (§5.1) — so dropping an all-default element would change the decoded
    ///   length, not just the bytes;
    /// - an array field already known to **differ from a non-empty declared
    ///   `default`**: absence would reconstruct that default, so the empty frame is
    ///   the only encoding of "explicitly empty" (§2, §3).
    ///
    /// The two failure directions are not symmetric, which is why this is the safe
    /// choice when in doubt: using it where [`OStream::write_sequence_end`] would
    /// do costs one non-canonical empty frame that a decoder normalizes away, while
    /// the reverse silently changes an array's length.
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_end_keep(&mut self) -> Result<()> {
        // The held-back run is committed by `write_id_type` itself — this closer
        // is an ordinary write, so it needs no commit of its own.
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
}

/// Unsigned integer element that can be widened to the wire value type.
#[cfg(feature = "array")]
pub trait UnsignedElem: Copy {
    /// Zero-extend to [`Unsigned`].
    fn widen(self) -> Unsigned;
}

/// Signed integer element that can be widened to the wire value type.
#[cfg(feature = "array")]
pub trait SignedElem: Copy {
    /// Sign-extend to [`Signed`].
    fn widen(self) -> Signed;
}

#[cfg(feature = "array")]
macro_rules! impl_unsigned_elem {
    ($($t:ty),*) => {$(
        impl UnsignedElem for $t {
            #[inline]
            fn widen(self) -> Unsigned { self as Unsigned }
        }
    )*};
}

#[cfg(feature = "array")]
macro_rules! impl_signed_elem {
    ($($t:ty),*) => {$(
        impl SignedElem for $t {
            #[inline]
            fn widen(self) -> Signed { self as Signed }
        }
    )*};
}

#[cfg(feature = "array")]
impl_unsigned_elem!(u8, u16, u32);
#[cfg(all(feature = "array", feature = "value64"))]
impl_unsigned_elem!(u64);
#[cfg(feature = "array")]
impl_signed_elem!(i8, i16, i32);
#[cfg(all(feature = "array", feature = "value64"))]
impl_signed_elem!(i64);
