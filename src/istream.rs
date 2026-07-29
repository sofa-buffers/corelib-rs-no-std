//! Streaming input stream decoder (port of `istream.c`).
//!
//! [`IStream`] is a byte-at-a-time state machine. Feed it arbitrary chunks with
//! [`IStream::feed`]; it parses field headers and pushes decoded fields to your
//! [`Visitor`]. Scalars and floats are delivered whole; string/blob payloads are
//! delivered in chunks (so they may exceed RAM); array elements are announced
//! with [`Visitor::array_begin`] and then delivered through the scalar/float
//! callbacks.
//!
//! Unlike the C decoder there is no per-field "bind a destination" step and no
//! explicit skip bookkeeping: a [`Visitor`] simply ignores fields it does not
//! care about. This keeps the port `unsafe`-free while preserving streaming.

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::{zigzag_decode, VarintDecoder};
use crate::{Id, Signed, Unsigned};

#[cfg(feature = "array")]
use crate::ArrayKind;
#[cfg(feature = "fixlen")]
use crate::FixlenType;

/// Receives decoded fields from an [`IStream`].
///
/// Every method has a default empty implementation, so an implementor overrides
/// only the field kinds it cares about. Fields that are not handled are simply
/// dropped (the equivalent of "not interested" / skip in the C API).
#[allow(unused_variables)]
pub trait Visitor {
    /// An unsigned integer field, or an unsigned array element.
    fn unsigned(&mut self, id: Id, value: Unsigned) {}

    /// A signed integer field, or a signed array element.
    fn signed(&mut self, id: Id, value: Signed) {}

    /// A 32-bit float field, or an `fp32` array element.
    #[cfg(feature = "fixlen")]
    fn fp32(&mut self, id: Id, value: f32) {}

    /// A 64-bit float field, or an `fp64` array element.
    #[cfg(feature = "fp64")]
    fn fp64(&mut self, id: Id, value: f64) {}

    /// A chunk of a string field. `total` is the full field length; `offset` is
    /// the byte position of this `chunk` within the field. For an empty string
    /// this is called once with `total == 0` and an empty `chunk`.
    ///
    /// The bytes are delivered **raw**: the corelib does not validate UTF-8 or
    /// build a `str`/`String`. A strict consumer (generated code) materializes
    /// the field with `core::str::from_utf8` and reports invalid bytes as
    /// [`Error::InvalidMsg`] — never replacing them with `U+FFFD` or truncating
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). `blob` payloads are opaque and never
    /// UTF-8-checked.
    #[cfg(feature = "fixlen")]
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// A chunk of a blob field. See [`Visitor::string`] for the chunking model.
    #[cfg(feature = "fixlen")]
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// Start of an array field with `count` elements of the given `kind`. The
    /// elements follow via the scalar / float callbacks with the same `id`.
    ///
    /// Fired exactly **once** per array field, never per element, and always
    /// before the first element and before the array's last element completes.
    /// For an integer array (`VARINTARRAY_UNSIGNED` / `VARINTARRAY_SIGNED`) it
    /// fires as soon as the count varint is read — that is the whole header. For
    /// a fixlen array it fires only after the `fixlen_word` has been read *and*
    /// validated, so `kind` names the element subtype ([`ArrayKind::Fp32`] /
    /// [`ArrayKind::Fp64`]) actually on the wire (CORELIB_PLAN §4.8 step 2). A
    /// consumer that compares `kind` against a declared element type therefore
    /// learns the subtype before it decides anything about the field: a
    /// contradicting subtype means the field is skipped whole (§4.8 step 3,
    /// MESSAGE_SPEC §7.3) and any schema bound on `count` must *not* be applied
    /// to it, because the field was never this array's value.
    ///
    /// The format ceiling on `count` (`ARRAY_MAX`) and the receiver's own array
    /// limits are unaffected by that ordering: they are checked on the count
    /// varint, before this call, and nothing is allocated on the strength of a
    /// count that has not passed them.
    ///
    /// A zero-count array is announced too (with `count == 0` and, for a fixlen
    /// array, the subtype from its `fixlen_word`); no element callback follows.
    #[cfg(feature = "array")]
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {}

    /// Start of a nested sequence with the given field `id`.
    #[cfg(feature = "sequence")]
    fn sequence_begin(&mut self, id: Id) {}

    /// End of the current nested sequence.
    #[cfg(feature = "sequence")]
    fn sequence_end(&mut self) {}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    VarintUnsigned,
    VarintSigned,
    #[cfg(feature = "fixlen")]
    FixlenLen,
    #[cfg(feature = "fixlen")]
    FixlenVal,
    #[cfg(feature = "fixlen")]
    FixlenRaw,
    #[cfg(feature = "array")]
    ArrayCount,
}

/// Streaming Sofab decoder.
pub struct IStream {
    varint: VarintDecoder,
    state: State,
    id: Id,

    // array context
    #[cfg(feature = "array")]
    array_kind: ArrayKind,
    #[cfg(feature = "array")]
    array_remaining: usize,
    #[cfg(feature = "array")]
    in_array: bool,
    /// The array being decoded is a fixlen array, so its element kind is still
    /// unknown while the count varint is read: it arrives with the
    /// `fixlen_word`, and `array_kind` is only meaningful from then on.
    #[cfg(all(feature = "array", feature = "fixlen"))]
    array_fixlen: bool,

    // fixlen context
    #[cfg(feature = "fixlen")]
    fixlen_type: FixlenType,
    #[cfg(feature = "fixlen")]
    fixlen_total: usize,
    #[cfg(feature = "fixlen")]
    fixlen_remaining: usize,
    #[cfg(feature = "fixlen")]
    acc: [u8; 8],

    // sequence nesting depth (for balanced start/end validation)
    #[cfg(feature = "sequence")]
    depth: u32,
}

impl Default for IStream {
    fn default() -> Self {
        Self::new()
    }
}

impl IStream {
    /// Create a fresh decoder ready to accept a new message.
    pub const fn new() -> Self {
        IStream {
            varint: VarintDecoder::new(),
            state: State::Idle,
            id: 0,
            #[cfg(feature = "array")]
            array_kind: ArrayKind::Unsigned,
            #[cfg(feature = "array")]
            array_remaining: 0,
            #[cfg(feature = "array")]
            in_array: false,
            #[cfg(all(feature = "array", feature = "fixlen"))]
            array_fixlen: false,
            #[cfg(feature = "fixlen")]
            fixlen_type: FixlenType::Fp32,
            #[cfg(feature = "fixlen")]
            fixlen_total: 0,
            #[cfg(feature = "fixlen")]
            fixlen_remaining: 0,
            #[cfg(feature = "fixlen")]
            acc: [0; 8],
            #[cfg(feature = "sequence")]
            depth: 0,
        }
    }

    /// Feed a chunk of encoded bytes, pushing decoded fields to `visitor`, and
    /// report the three-valued decode outcome of everything consumed *so far*
    /// (`MESSAGE_SPEC.md` §7). The same status holds for a one-shot `feed` of a
    /// whole message and for each `feed` of a streamed chunk sequence:
    ///
    /// * `Ok(())` — **`COMPLETE`**: the consumed bytes end **exactly** at a
    ///   field boundary; a valid message may end here (more fields may follow).
    /// * [`Err(Error::Incomplete)`](Error::Incomplete) — **`INCOMPLETE`**: the
    ///   bytes end **inside** a field (an unterminated varint, a fixlen / string
    ///   / blob payload short of its declared length) or with a sequence still
    ///   open. Not an error — the partial tail is retained and feeding more
    ///   bytes may complete it. End-of-input is the caller's decision, so there
    ///   is no `finish`/`finalize` step.
    /// * [`Err(Error::InvalidMsg)`](Error::InvalidMsg) — **`INVALID`**: the
    ///   bytes are malformed regardless of what follows (varint overflow, bad
    ///   type tag, oversized length/count, nesting past `MAX_DEPTH`, dangling
    ///   sequence end). Terminal.
    ///
    /// Decoding can continue across many `feed` calls; the decoder keeps all
    /// state internally.
    pub fn feed<V: Visitor>(&mut self, data: &[u8], visitor: &mut V) -> Result<()> {
        let mut i = 0;
        while i < data.len() {
            // Fast path: stream string/blob payloads in bulk rather than
            // one callback per byte.
            #[cfg(feature = "fixlen")]
            if self.state == State::FixlenRaw {
                // Slice the remaining input first, then cap by `fixlen_remaining`;
                // `min` makes `take <= rest.len()`, so the chunk slice carries no
                // panicking bounds check.
                let rest = &data[i..];
                let take = rest.len().min(self.fixlen_remaining);
                let offset = self.fixlen_total - self.fixlen_remaining;
                let chunk = &rest[..take];
                match self.fixlen_type {
                    FixlenType::Str => visitor.string(self.id, self.fixlen_total, offset, chunk),
                    FixlenType::Blob => visitor.blob(self.id, self.fixlen_total, offset, chunk),
                    _ => return Err(Error::InvalidMsg),
                }
                self.fixlen_remaining -= take;
                i += take;
                if self.fixlen_remaining == 0 {
                    self.state = State::Idle;
                }
                continue;
            }

            self.step(data[i], visitor)?;
            i += 1;
        }

        // §7: the outcome is a property of the bytes consumed so far, read
        // straight off the decoder's own state — no separate finalization gate.
        // Malformed input already returned `Err(Error::InvalidMsg)` above via
        // `?`; reaching here means the bytes are well-formed, so they are either
        // `COMPLETE` (at a field boundary) or `INCOMPLETE` (mid-field / open
        // sequence). We surface `INCOMPLETE` distinctly instead of silently
        // accepting a partial tail as a finished message.
        if self.at_field_boundary() {
            Ok(())
        } else {
            Err(Error::Incomplete)
        }
    }

    /// True when the decoder sits **exactly** at a top-level field boundary: no
    /// half-read header/value varint, no fixlen / string / blob / array payload
    /// in progress, and no sequence left open. This is the only state from which
    /// the consumed bytes form a `COMPLETE` message (§7); any other state means
    /// the bytes end mid-field or with an open sequence and is `INCOMPLETE`.
    fn at_field_boundary(&self) -> bool {
        if self.state != State::Idle || self.varint.is_pending() {
            // Mid-value, mid-payload, or a partial header varint pending.
            return false;
        }
        #[cfg(feature = "sequence")]
        if self.depth != 0 {
            // A sequence-start with no matching sequence-end yet.
            return false;
        }
        true
    }

    fn step<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        // `FixlenVal` is the only byte-oriented state (it copies raw payload
        // bytes); `FixlenRaw` is drained by `feed`'s bulk path and never reaches
        // here. Every remaining state is introduced by a leading varint, so the
        // push-a-byte / "need more" dance is decoded **once** here and the
        // completed value dispatched below — rather than repeated per state.
        #[cfg(feature = "fixlen")]
        if self.state == State::FixlenVal {
            return self.step_fixlen_val(byte, visitor);
        }

        let value = match self.varint.push(byte)? {
            Some(v) => v,
            None => return Ok(()),
        };

        match self.state {
            State::Idle => self.on_header(value, visitor),
            State::VarintUnsigned => {
                visitor.unsigned(self.id, value);
                self.advance_after_element();
                Ok(())
            }
            State::VarintSigned => {
                visitor.signed(self.id, zigzag_decode(value));
                self.advance_after_element();
                Ok(())
            }
            #[cfg(feature = "fixlen")]
            State::FixlenLen => self.on_fixlen_len(value, visitor),
            #[cfg(feature = "array")]
            State::ArrayCount => self.on_array_count(value, visitor),
            // Handled before the varint decode (`FixlenVal`) or in `feed`
            // (`FixlenRaw`); these arms just keep the match exhaustive without a
            // panicking `unreachable!`.
            #[cfg(feature = "fixlen")]
            State::FixlenVal | State::FixlenRaw => Ok(()),
        }
    }

    #[cfg_attr(not(feature = "sequence"), allow(unused_variables))]
    fn on_header<V: Visitor>(&mut self, header: Unsigned, visitor: &mut V) -> Result<()> {
        let wire_type = (header & 0x07) as u8;
        let id = header >> 3;
        if id > ID_MAX as Unsigned {
            return Err(Error::InvalidMsg);
        }
        self.id = id as Id;
        #[cfg(feature = "array")]
        {
            self.in_array = false;
        }
        #[cfg(all(feature = "array", feature = "fixlen"))]
        {
            self.array_fixlen = false;
        }

        match wire_type {
            T_VARINT_UNSIGNED => self.state = State::VarintUnsigned,
            T_VARINT_SIGNED => self.state = State::VarintSigned,

            #[cfg(feature = "fixlen")]
            T_FIXLEN => self.state = State::FixlenLen,

            #[cfg(feature = "array")]
            T_VARINTARRAY_UNSIGNED => {
                self.array_kind = ArrayKind::Unsigned;
                self.state = State::ArrayCount;
            }
            #[cfg(feature = "array")]
            T_VARINTARRAY_SIGNED => {
                self.array_kind = ArrayKind::Signed;
                self.state = State::ArrayCount;
            }
            #[cfg(all(feature = "array", feature = "fixlen"))]
            T_FIXLENARRAY => {
                // The element kind is not known yet — it is carried by the
                // `fixlen_word` that follows the count (§4.8), so `array_kind`
                // is set (and the array announced) in `on_fixlen_len`.
                self.array_fixlen = true;
                self.state = State::ArrayCount;
            }

            #[cfg(feature = "sequence")]
            T_SEQUENCE_START => {
                // Reject nesting beyond the normative MAX_DEPTH (§4.9/§6.2).
                if self.depth >= MAX_DEPTH {
                    return Err(Error::InvalidMsg);
                }
                self.depth += 1;
                visitor.sequence_begin(self.id);
                // stays in Idle
            }
            #[cfg(feature = "sequence")]
            T_SEQUENCE_END => {
                if self.depth == 0 {
                    return Err(Error::InvalidMsg);
                }
                self.depth -= 1;
                visitor.sequence_end();
                // stays in Idle
            }

            _ => return Err(Error::InvalidMsg),
        }
        Ok(())
    }

    /// Shared "next element or back to idle" logic for varint scalars/arrays.
    #[inline]
    fn advance_after_element(&mut self) {
        #[cfg(feature = "array")]
        if self.in_array {
            self.array_remaining -= 1;
            if self.array_remaining > 0 {
                return; // stay in the same state for the next element
            }
            self.in_array = false;
        }
        self.state = State::Idle;
    }

    #[cfg(feature = "fixlen")]
    fn on_fixlen_len<V: Visitor>(&mut self, header: Unsigned, visitor: &mut V) -> Result<()> {
        let subtype = FixlenType::from_raw((header & 0x07) as u8)?;
        let length = (header >> 3) as usize;
        // Reject implausibly large fixlen lengths (matches SOFAB_FIXLEN_MAX).
        if header >> 3 > ARRAY_MAX {
            return Err(Error::InvalidMsg);
        }

        self.fixlen_type = subtype;
        self.fixlen_total = length;
        self.fixlen_remaining = length;

        // The second header word of a fixlen array (§4.8). It carries the
        // element subtype, so this — not the count word — is where the array is
        // announced to the visitor.
        #[cfg(feature = "array")]
        if self.in_array {
            // Format first: an array element must be a fixed-width subtype whose
            // per-element length matches it. A string/blob subtype, or an fp32
            // that is not 4 bytes / an fp64 that is not 8, is malformed outright
            // (§4.8 allows only fixed-width subtypes here) — that is a format
            // violation, never a §7.3 schema-mismatch skip, so it is rejected
            // before the visitor hears about the array at all.
            let kind = match subtype {
                FixlenType::Fp32 if length == 4 => ArrayKind::Fp32,
                #[cfg(feature = "fp64")]
                FixlenType::Fp64 if length == 8 => ArrayKind::Fp64,
                _ => return Err(Error::InvalidMsg),
            };
            self.array_kind = kind;
            // §4.8 step 2/3: the subtype is known, so the consumer can compare
            // it against a declared element type and skip the whole field
            // before any schema bound on `count` comes into play.
            visitor.array_begin(self.id, kind, self.array_remaining);

            if self.array_remaining == 0 {
                // An empty fixlen array still carries its `fixlen_word` (so an
                // empty fp32 stays distinct from an empty fp64), but no payload
                // follows: resume at the next field without entering `FixlenVal`.
                self.in_array = false;
                self.state = State::Idle;
            } else {
                self.state = State::FixlenVal;
            }
            return Ok(());
        }

        match subtype {
            FixlenType::Fp32 => {
                if length != 4 {
                    return Err(Error::InvalidMsg);
                }
                self.state = State::FixlenVal;
            }
            #[cfg(feature = "fp64")]
            FixlenType::Fp64 => {
                if length != 8 {
                    return Err(Error::InvalidMsg);
                }
                self.state = State::FixlenVal;
            }
            FixlenType::Str | FixlenType::Blob => {
                // A scalar string/blob field: the array case (where these
                // subtypes are illegal) already returned above.
                if length == 0 {
                    match subtype {
                        FixlenType::Str => visitor.string(self.id, 0, 0, &[]),
                        FixlenType::Blob => visitor.blob(self.id, 0, 0, &[]),
                        _ => unreachable!(),
                    }
                    self.state = State::Idle;
                } else {
                    self.state = State::FixlenRaw;
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "fixlen")]
    fn step_fixlen_val<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        // Byte position within the value = bytes already accumulated. The `& 7`
        // is a no-op on the value (`fixlen_total` is 4 or 8 here) but proves the
        // index in-bounds so no panicking bounds check is emitted.
        self.acc[(self.fixlen_total - self.fixlen_remaining) & 7] = byte;
        self.fixlen_remaining -= 1;
        if self.fixlen_remaining != 0 {
            return Ok(());
        }

        match self.fixlen_type {
            FixlenType::Fp32 => {
                let bytes = [self.acc[0], self.acc[1], self.acc[2], self.acc[3]];
                visitor.fp32(self.id, f32::from_le_bytes(bytes));
            }
            #[cfg(feature = "fp64")]
            FixlenType::Fp64 => {
                visitor.fp64(self.id, f64::from_le_bytes(self.acc));
            }
            _ => return Err(Error::InvalidMsg),
        }

        // Next array element (reuse the element size) or back to idle.
        #[cfg(feature = "array")]
        if self.in_array {
            self.array_remaining -= 1;
            if self.array_remaining > 0 {
                self.fixlen_remaining = self.fixlen_total;
                return Ok(());
            }
            self.in_array = false;
        }
        self.state = State::Idle;
        Ok(())
    }

    #[cfg(feature = "array")]
    fn on_array_count<V: Visitor>(&mut self, count: Unsigned, visitor: &mut V) -> Result<()> {
        // §4.8 step 1: the *format* ceiling is enforced here, on the count word,
        // whatever the element subtype turns out to be — and nothing is sized or
        // announced on the strength of a count that fails it.
        if count > ARRAY_MAX {
            return Err(Error::InvalidMsg);
        }
        let count = count as usize;

        // A fixlen array carries a second header word, the `fixlen_word`, and
        // its element subtype only becomes known there (§4.8 step 2). The array
        // is therefore *not* announced here: `on_fixlen_len` fires
        // `array_begin` once it has the subtype, so a consumer can decide the
        // field is skippable (§7.3) before applying any schema bound to `count`.
        // This holds for `count == 0` too — an empty fixlen array still carries
        // its word, so an empty fp32 stays distinct from an empty fp64 — and it
        // is what makes a message truncated *between* the two words INCOMPLETE
        // rather than judged on the count alone.
        #[cfg(feature = "fixlen")]
        if self.array_fixlen {
            self.array_remaining = count;
            self.in_array = true;
            self.state = State::FixlenLen;
            return Ok(());
        }

        // An integer array's header is complete at the count word: there is no
        // second word, so the array is announced right away, as before.
        visitor.array_begin(self.id, self.array_kind, count);

        // A zero-count integer array is exactly `[ header ][ count = 0 ]` and
        // resumes at the next field (§4.7).
        if count == 0 {
            self.in_array = false;
            self.state = State::Idle;
            return Ok(());
        }

        self.array_remaining = count;
        self.in_array = true;
        // Only the two integer kinds can reach this point; a fixlen array
        // returned above.
        self.state = if self.array_kind == ArrayKind::Signed {
            State::VarintSigned
        } else {
            State::VarintUnsigned
        };
        Ok(())
    }
}
