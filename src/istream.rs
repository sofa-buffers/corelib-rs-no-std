//! Streaming input stream decoder (port of `istream.c`).
//!
//! [`IStream`] is a byte-at-a-time state machine. Feed it arbitrary chunks with
//! [`IStream::feed`]; it parses field headers and pushes decoded fields to your
//! [`Visitor`]. Scalars and floats are delivered whole; a scalar fixlen field is
//! announced at its length word with [`Visitor::fixlen_begin`] and its
//! string/blob payload then delivered in chunks (so it may exceed RAM); array
//! elements are announced with [`Visitor::array_begin`] and then delivered
//! through the scalar/float callbacks.
//!
//! Unlike the C decoder there is no per-field "bind a destination" step and no
//! explicit skip bookkeeping: a [`Visitor`] simply ignores fields it does not
//! care about. This keeps the port `unsafe`-free while preserving streaming.

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::{zigzag_decode, VALUE_BITS};
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

    /// Start of a scalar fixlen field, announced after its length word is read
    /// and validated and **before** any payload byte. Fired exactly **once** per
    /// field, `total == 0` included, and never for an array element (an array is
    /// announced through [`Visitor::array_begin`] instead).
    ///
    /// This is the scalar twin of `array_begin`, and exists for the same reason:
    /// a schema bound established by the length word alone — a string/blob whose
    /// `total` exceeds a `maxlen` — must be latchable *at the word*. CORELIB_PLAN
    /// §5.2 makes INVALID dominate INCOMPLETE, so a message truncated exactly at
    /// the length word cannot be allowed to degrade to INCOMPLETE while the same
    /// bytes read whole are INVALID; without this callback the only event
    /// carrying `total` is [`Visitor::string`] / [`Visitor::blob`], which cannot
    /// fire for a message that ends there. Raising from this callback is what a
    /// consumer uses to turn the field INVALID at the word.
    ///
    /// `subtype` is the subtype actually on the wire (string / blob / fp32 /
    /// fp64): the corelib knows what *arrived*, not what was *declared*, so a
    /// consumer whose field expects a different subtype treats this as a §7.3
    /// skip rather than measuring `total` against that field's bound.
    #[cfg(feature = "fixlen")]
    fn fixlen_begin(&mut self, id: Id, subtype: FixlenType, total: usize) {}

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
    /// Terminal: the bytes consumed so far are malformed. `INVALID` is terminal
    /// (§5.2) — no continuation can make them valid — and most of the conditions
    /// that produce it (a dangling sequence end, an overlong varint, an
    /// over-maximum id) leave the machine at an otherwise clean field boundary,
    /// so the verdict has to be *remembered* rather than recomputed: without
    /// this state the next `feed` would parse on and report `COMPLETE` for a
    /// message the decoder itself has already rejected.
    ///
    /// It lives in `state` rather than in a flag of its own precisely because
    /// this is a state the machine never leaves: an extra byte in `Core` would
    /// be free in the struct's padding but not in flash — it perturbs the
    /// initializer image `IStream::new` stores, which on a 64-bit-value build
    /// costs ~180 B of `.text`, an order of magnitude more than reusing the
    /// state byte.
    Invalid,
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

/// The decoder's per-byte state, grouped into one struct.
///
/// `acc` is 64 bits on a `value64` build, so this struct is 8-byte aligned and
/// would carry 7 bytes of tail padding; the mode flags live there **for free**.
/// That packing is what keeps [`IStream`] at 32 bytes with every feature on —
/// the size at (and below) which the compiler zero-initializes it with inline
/// stores instead of linking a ~158-byte `__aeabi_memclr8` helper, which is a
/// bigger flash item than the state machine's own bookkeeping.
struct Core {
    /// Accumulator of the varint being decoded — and, on builds whose value
    /// type is wide enough, of the `fixlen` float payload being assembled
    /// (`float_push`): the two are never in flight at the same time, so they
    /// share one word instead of costing a second one.
    acc: Unsigned,
    /// Payload bits already in `acc`. Non-zero exactly while a varint is
    /// mid-decode, which is how an unterminated tail is told from a clean field
    /// boundary.
    shift: u8,
    state: State,

    #[cfg(feature = "array")]
    array_kind: ArrayKind,
    #[cfg(feature = "array")]
    in_array: bool,
    /// The array being decoded is a fixlen array, so its element kind is still
    /// unknown while the count varint is read: it arrives with the
    /// `fixlen_word`, and `array_kind` is only meaningful from then on.
    #[cfg(all(feature = "array", feature = "fixlen"))]
    array_fixlen: bool,

    #[cfg(feature = "fixlen")]
    fixlen_type: FixlenType,

    /// Sequence nesting depth (for balanced start/end validation). `MAX_DEPTH`
    /// is 255, so a byte holds every legal depth.
    #[cfg(feature = "sequence")]
    depth: u8,
}

/// Streaming Sofab decoder.
pub struct IStream {
    core: Core,
    id: Id,

    // array context
    #[cfg(feature = "array")]
    array_remaining: usize,

    // fixlen context
    #[cfg(feature = "fixlen")]
    fixlen_total: usize,
    #[cfg(feature = "fixlen")]
    fixlen_remaining: usize,

    /// Low half of the float accumulator, for the one build shape whose value
    /// word cannot hold a whole `fp64` payload: 8 payload bytes against a
    /// 4-byte `Core::acc` when `value64` is off. The payload is then assembled
    /// in the two-word window `(acc_lo, Core::acc)` — one extra word, not a
    /// separate 8-byte buffer.
    #[cfg(all(feature = "fp64", not(feature = "value64")))]
    acc_lo: u32,
}

impl Core {
    /// Feed one byte into the varint currently being decoded.
    ///
    /// * `Ok(Some(v))` — a complete value was decoded (state auto-resets).
    /// * `Ok(None)` — more bytes are needed.
    /// * `Err(InvalidMsg)` — the varint is longer than the value type allows.
    ///
    /// `inline(never)`: this is the per-byte prologue of every decoder state,
    /// reached from the monomorphized [`IStream::step`]. Left to LTO it gets
    /// inlined into each visitor instantiation's state machine, costing ~1 KB
    /// of flash in a generated-code decoder for a ~50 B saving in the
    /// synthetic probe. Keeping it outlined shares one copy across all states
    /// and visitors, and borrowing only `Core` (not the whole `IStream`) leaves
    /// the surrounding fields promotable to registers.
    #[inline(never)]
    fn push(&mut self, byte: u8) -> Result<Option<Unsigned>> {
        // Reject an overlong (>value-width) varint before it silently truncates
        // (§4.1/§6.3). On the final byte that fills the value, only the low
        // `room` payload bits fit below the value width; any higher bit is a
        // >64-bit overflow. This matches corelib-c-cpp (`istream.c`),
        // corelib-rs (`varint.rs`) and corelib-zig — where this port previously
        // discarded the spilling bits and returned a corrupted value.
        //
        // The shift is kept as a byte in `Core` (it has to fit the tail padding
        // there) but computed in the machine's natural width, so the arithmetic
        // costs no repeated byte-truncation.
        let shift = u32::from(self.shift);
        let room = u32::from(VALUE_BITS) - shift; // payload bits below the width
        if room < 7 && u32::from(byte & 0x7F) >> room != 0 {
            self.acc = 0;
            self.shift = 0;
            return Err(Error::InvalidMsg);
        }

        // OR in the 7 payload bits at the current position.
        self.acc |= ((byte & 0x7F) as Unsigned) << shift;
        self.shift = (shift + 7) as u8;

        if byte & 0x80 == 0 {
            let v = self.acc;
            self.acc = 0;
            self.shift = 0;
            return Ok(Some(v));
        }

        // Continuation bit set but no more room -> overflow.
        if shift + 7 >= u32::from(VALUE_BITS) {
            self.acc = 0;
            self.shift = 0;
            return Err(Error::InvalidMsg);
        }

        Ok(None)
    }
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
            core: Core {
                acc: 0,
                shift: 0,
                state: State::Idle,
                #[cfg(feature = "array")]
                array_kind: ArrayKind::Unsigned,
                #[cfg(feature = "array")]
                in_array: false,
                #[cfg(all(feature = "array", feature = "fixlen"))]
                array_fixlen: false,
                // Any subtype will do — `on_fixlen_len` sets it before anything
                // reads it — and a non-zero one keeps the whole struct from
                // being an all-zero image, which the compiler would otherwise
                // materialize by calling a ~158-byte `__aeabi_memclr8` helper
                // instead of storing the few words inline.
                #[cfg(feature = "fixlen")]
                fixlen_type: FixlenType::Blob,
                #[cfg(feature = "sequence")]
                depth: 0,
            },
            id: 0,
            #[cfg(feature = "array")]
            array_remaining: 0,
            #[cfg(feature = "fixlen")]
            fixlen_total: 0,
            #[cfg(feature = "fixlen")]
            fixlen_remaining: 0,
            #[cfg(all(feature = "fp64", not(feature = "value64")))]
            acc_lo: 0,
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
    ///   sequence end). **Terminal**, and latched: no continuation can make
    ///   these bytes valid, so every later `feed` on this decoder returns
    ///   `InvalidMsg` again without consuming the chunk or delivering a single
    ///   further field to the visitor. Decoding another message means a fresh
    ///   [`IStream::new`] — where to resynchronize the byte stream is the
    ///   caller's framing decision, not the decoder's.
    ///
    /// Decoding can continue across many `feed` calls; the decoder keeps all
    /// state internally. Because the verdict is latched, it does not depend on
    /// where the chunk boundaries fall: feeding a stream one byte at a time
    /// yields the same outcome as feeding it whole.
    pub fn feed<V: Visitor>(&mut self, data: &[u8], visitor: &mut V) -> Result<()> {
        // §5.2: `INVALID` is terminal. This is what keeps the next chunk from
        // being parsed as if the message were still intact.
        if self.core.state == State::Invalid {
            return Err(Error::InvalidMsg);
        }
        let mut i = 0;
        while i < data.len() {
            // Fast path: stream string/blob payloads in bulk rather than
            // one callback per byte.
            #[cfg(feature = "fixlen")]
            if self.core.state == State::FixlenRaw {
                // Slice the remaining input first, then cap by `fixlen_remaining`;
                // `min` makes `take <= rest.len()`, so the chunk slice carries no
                // panicking bounds check.
                let rest = &data[i..];
                let take = rest.len().min(self.fixlen_remaining);
                let offset = self.fixlen_total - self.fixlen_remaining;
                let chunk = &rest[..take];
                match self.core.fixlen_type {
                    FixlenType::Str => visitor.string(self.id, self.fixlen_total, offset, chunk),
                    FixlenType::Blob => visitor.blob(self.id, self.fixlen_total, offset, chunk),
                    _ => return Err(self.latch(Error::InvalidMsg)),
                }
                self.fixlen_remaining -= take;
                i += take;
                if self.fixlen_remaining == 0 {
                    self.core.state = State::Idle;
                }
                continue;
            }

            if let Err(e) = self.step(data[i], visitor) {
                return Err(self.latch(e));
            }
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

    /// Enter the terminal [`State::Invalid`] and hand the error straight back,
    /// so every site that produces one is a single `return Err(self.latch(e))`.
    ///
    /// Only [`Error::InvalidMsg`] latches: it is the one outcome §5.2 declares
    /// terminal. `Incomplete` never reaches here — it is computed from the state
    /// after the loop, never returned by a step — and must not be latched even
    /// if it ever did: feeding more bytes is exactly how it is resolved.
    ///
    /// `cold` + `inline(never)`: this runs once per broken message, on the way
    /// out, and keeping it out of the per-byte loop's body leaves the loop (and
    /// its register allocation) as it was.
    #[cold]
    #[inline(never)]
    fn latch(&mut self, e: Error) -> Error {
        if e == Error::InvalidMsg {
            self.core.state = State::Invalid;
        }
        e
    }

    /// True when the decoder sits **exactly** at a top-level field boundary: no
    /// half-read header/value varint, no fixlen / string / blob / array payload
    /// in progress, and no sequence left open. This is the only state from which
    /// the consumed bytes form a `COMPLETE` message (§7); any other state means
    /// the bytes end mid-field or with an open sequence and is `INCOMPLETE`.
    fn at_field_boundary(&self) -> bool {
        if self.core.state != State::Idle || self.core.shift != 0 {
            // Mid-value, mid-payload, or a partial header varint pending.
            return false;
        }
        #[cfg(feature = "sequence")]
        if self.core.depth != 0 {
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
        if self.core.state == State::FixlenVal {
            return self.step_fixlen_val(byte, visitor);
        }

        let value = match self.core.push(byte)? {
            Some(v) => v,
            None => return Ok(()),
        };

        match self.core.state {
            State::Idle => self.on_header(value, visitor),
            // Both integer states end the same way — deliver, then take the
            // next element or leave the field — so they share that tail instead
            // of each carrying a copy of it.
            State::VarintUnsigned | State::VarintSigned => {
                if self.core.state == State::VarintSigned {
                    visitor.signed(self.id, zigzag_decode(value));
                } else {
                    visitor.unsigned(self.id, value);
                }
                self.advance_after_element();
                Ok(())
            }
            #[cfg(feature = "fixlen")]
            State::FixlenLen => self.on_fixlen_len(value, visitor),
            #[cfg(feature = "array")]
            State::ArrayCount => self.on_array_count(value, visitor),
            // Handled before the varint decode (`FixlenVal`), in `feed`'s bulk
            // path (`FixlenRaw`), or never reached at all because `feed` returns
            // at its first byte (`Invalid`); these arms just keep the match
            // exhaustive without a panicking `unreachable!`.
            State::Invalid => Ok(()),
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
            self.core.in_array = false;
        }
        #[cfg(all(feature = "array", feature = "fixlen"))]
        {
            self.core.array_fixlen = false;
        }

        match wire_type {
            T_VARINT_UNSIGNED => self.core.state = State::VarintUnsigned,
            T_VARINT_SIGNED => self.core.state = State::VarintSigned,

            #[cfg(feature = "fixlen")]
            T_FIXLEN => self.core.state = State::FixlenLen,

            #[cfg(feature = "array")]
            T_VARINTARRAY_UNSIGNED => {
                self.core.array_kind = ArrayKind::Unsigned;
                self.core.state = State::ArrayCount;
            }
            #[cfg(feature = "array")]
            T_VARINTARRAY_SIGNED => {
                self.core.array_kind = ArrayKind::Signed;
                self.core.state = State::ArrayCount;
            }
            #[cfg(all(feature = "array", feature = "fixlen"))]
            T_FIXLENARRAY => {
                // The element kind is not known yet — it is carried by the
                // `fixlen_word` that follows the count (§4.8), so `array_kind`
                // is set (and the array announced) in `on_fixlen_len`.
                self.core.array_fixlen = true;
                self.core.state = State::ArrayCount;
            }

            #[cfg(feature = "sequence")]
            T_SEQUENCE_START => {
                // Reject nesting beyond the normative MAX_DEPTH (§4.9/§6.2).
                if u32::from(self.core.depth) >= MAX_DEPTH {
                    return Err(Error::InvalidMsg);
                }
                self.core.depth += 1;
                visitor.sequence_begin(self.id);
                // stays in Idle
            }
            #[cfg(feature = "sequence")]
            T_SEQUENCE_END => {
                if self.core.depth == 0 {
                    return Err(Error::InvalidMsg);
                }
                self.core.depth -= 1;
                visitor.sequence_end();
                // stays in Idle
            }

            _ => return Err(Error::InvalidMsg),
        }
        Ok(())
    }

    /// Shared "next element or back to idle" logic, for every element kind.
    ///
    /// Returns `true` when another array element follows — the decoder then
    /// stays in the state that reads one — and otherwise leaves it idle at the
    /// next field boundary.
    #[inline]
    fn advance_after_element(&mut self) -> bool {
        #[cfg(feature = "array")]
        if self.core.in_array {
            self.array_remaining -= 1;
            if self.array_remaining > 0 {
                return true;
            }
            self.core.in_array = false;
        }
        self.core.state = State::Idle;
        false
    }

    #[cfg(feature = "fixlen")]
    fn on_fixlen_len<V: Visitor>(&mut self, header: Unsigned, visitor: &mut V) -> Result<()> {
        let subtype = FixlenType::from_raw((header & 0x07) as u8)?;
        let length = (header >> 3) as usize;
        // Reject implausibly large fixlen lengths (matches SOFAB_FIXLEN_MAX).
        if header >> 3 > ARRAY_MAX {
            return Err(Error::InvalidMsg);
        }

        self.core.fixlen_type = subtype;
        self.fixlen_total = length;
        self.fixlen_remaining = length;

        // The float subtypes have exactly one legal payload width each; the
        // dynamic ones (string / blob) take whatever length the word declares.
        // Deciding that once here — rather than per subtype in both branches
        // below — turns the wrong-width rejection (§4.6) and the fixed-width
        // rule for array elements (§4.8) into plain comparisons.
        let width = match subtype {
            FixlenType::Fp32 => 4,
            #[cfg(feature = "fp64")]
            FixlenType::Fp64 => 8,
            FixlenType::Str | FixlenType::Blob => 0,
        };
        let legal_float = width != 0 && length == width;

        // The second header word of a fixlen array (§4.8). It carries the
        // element subtype, so this — not the count word — is where the array is
        // announced to the visitor.
        #[cfg(feature = "array")]
        if self.core.in_array {
            // Format first: an array element must be a fixed-width subtype whose
            // per-element length matches it. A string/blob subtype, or an fp32
            // that is not 4 bytes / an fp64 that is not 8, is malformed outright
            // (§4.8 allows only fixed-width subtypes here) — that is a format
            // violation, never a §7.3 schema-mismatch skip, so it is rejected
            // before the visitor hears about the array at all.
            if !legal_float {
                return Err(Error::InvalidMsg);
            }
            // The two legal widths are the two float kinds, one to one.
            #[cfg(feature = "fp64")]
            let kind = if width == 4 {
                ArrayKind::Fp32
            } else {
                ArrayKind::Fp64
            };
            #[cfg(not(feature = "fp64"))]
            let kind = ArrayKind::Fp32;
            self.core.array_kind = kind;
            // §4.8 step 2/3: the subtype is known, so the consumer can compare
            // it against a declared element type and skip the whole field
            // before any schema bound on `count` comes into play.
            visitor.array_begin(self.id, kind, self.array_remaining);

            if self.array_remaining == 0 {
                // An empty fixlen array still carries its `fixlen_word` (so an
                // empty fp32 stays distinct from an empty fp64), but no payload
                // follows: resume at the next field without entering `FixlenVal`.
                self.core.in_array = false;
                self.core.state = State::Idle;
            } else {
                self.core.state = State::FixlenVal;
            }
            return Ok(());
        }

        // A `fixlen_word` declaring any other length for fp32 / fp64 is
        // malformed and must be rejected here, before the header hook fires or
        // any payload byte is consumed or waited for (§4.6, §5.2).
        if width != 0 && !legal_float {
            return Err(Error::InvalidMsg);
        }

        // Announce the scalar field at its length word — after the word is read
        // and validated, before any payload byte — so a schema `maxlen`
        // violation is latchable *here* (CORELIB_PLAN §5.2, and see
        // [`Visitor::fixlen_begin`]): a message that ends exactly at this word
        // must stay INVALID rather than degrade to INCOMPLETE. This is the
        // scalar twin of the `array_begin` above; the array case has already
        // returned, so this fires once per scalar fixlen field, `total == 0`
        // included.
        visitor.fixlen_begin(self.id, subtype, length);

        if width != 0 {
            self.core.state = State::FixlenVal;
            return Ok(());
        }

        // A scalar string/blob field: the array case (where these subtypes are
        // illegal) already returned above.
        if length == 0 {
            // An empty string/blob has no payload to stream, so it is delivered
            // as the single zero-length chunk the callback contract promises.
            match subtype {
                FixlenType::Blob => visitor.blob(self.id, 0, 0, &[]),
                _ => visitor.string(self.id, 0, 0, &[]),
            }
            self.core.state = State::Idle;
        } else {
            self.core.state = State::FixlenRaw;
        }
        Ok(())
    }

    /// Absorb one byte of an `fp32` / `fp64` payload.
    ///
    /// Bytes arrive least-significant first and are shifted in from the top, so
    /// after `n` bytes the payload occupies the accumulator's top `8 * n` bits
    /// in wire order. Both shifts are by a constant, so this needs no
    /// variable-shift helper and no index that has to be proven in bounds.
    #[cfg(feature = "fixlen")]
    #[inline]
    fn float_push(&mut self, byte: u8) {
        #[cfg(any(feature = "value64", not(feature = "fp64")))]
        {
            self.core.acc = (self.core.acc >> 8) | ((byte as Unsigned) << (Unsigned::BITS - 8));
        }
        // Same shift, carried across the two-word window (see `acc_lo`): the
        // byte enters at the top of the high word and each word hands its
        // lowest byte down to the next.
        #[cfg(all(feature = "fp64", not(feature = "value64")))]
        {
            self.acc_lo = (self.acc_lo >> 8) | (self.core.acc << 24);
            self.core.acc = (self.core.acc >> 8) | ((byte as Unsigned) << 24);
        }
    }

    /// Take the assembled 4-byte payload as an `f32`, clearing the accumulator
    /// so the next varint starts from zero.
    #[cfg(feature = "fixlen")]
    #[inline]
    fn float_take_f32(&mut self) -> f32 {
        #[cfg(any(feature = "value64", not(feature = "fp64")))]
        {
            // The shift and the cast are both no-ops when the value type is
            // already 32-bit, which is exactly what makes one expression serve
            // both widths.
            #[allow(clippy::unnecessary_cast)]
            let bits = (self.core.acc >> (Unsigned::BITS - 32)) as u32;
            self.core.acc = 0;
            f32::from_bits(bits)
        }
        // Four bytes fill exactly the window's high word.
        #[cfg(all(feature = "fp64", not(feature = "value64")))]
        {
            let bits = self.core.acc;
            self.core.acc = 0;
            f32::from_bits(bits)
        }
    }

    /// Take the assembled 8-byte payload as an `f64`. See [`float_take_f32`].
    #[cfg(feature = "fp64")]
    #[inline]
    fn float_take_f64(&mut self) -> f64 {
        #[cfg(feature = "value64")]
        {
            let bits = self.core.acc;
            self.core.acc = 0;
            f64::from_bits(bits)
        }
        #[cfg(not(feature = "value64"))]
        {
            let bits = (u64::from(self.core.acc) << 32) | u64::from(self.acc_lo);
            self.core.acc = 0;
            self.acc_lo = 0;
            f64::from_bits(bits)
        }
    }

    #[cfg(feature = "fixlen")]
    fn step_fixlen_val<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        self.float_push(byte);
        self.fixlen_remaining -= 1;
        if self.fixlen_remaining != 0 {
            return Ok(());
        }

        // `FixlenVal` is only ever entered for a float subtype whose width
        // `on_fixlen_len` has already validated, so there is no third case to
        // reject here — and no unreachable error path to carry.
        match self.core.fixlen_type {
            #[cfg(feature = "fp64")]
            FixlenType::Fp64 => {
                let v = self.float_take_f64();
                visitor.fp64(self.id, v);
            }
            _ => {
                let v = self.float_take_f32();
                visitor.fp32(self.id, v);
            }
        }

        // A float array's next element reuses the width its `fixlen_word`
        // declared once for all of them (§4.8).
        if self.advance_after_element() {
            self.fixlen_remaining = self.fixlen_total;
        }
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
        if self.core.array_fixlen {
            self.array_remaining = count;
            self.core.in_array = true;
            self.core.state = State::FixlenLen;
            return Ok(());
        }

        // An integer array's header is complete at the count word: there is no
        // second word, so the array is announced right away, as before.
        visitor.array_begin(self.id, self.core.array_kind, count);

        // A zero-count integer array is exactly `[ header ][ count = 0 ]` and
        // resumes at the next field (§4.7).
        if count == 0 {
            self.core.in_array = false;
            self.core.state = State::Idle;
            return Ok(());
        }

        self.array_remaining = count;
        self.core.in_array = true;
        // Only the two integer kinds can reach this point; a fixlen array
        // returned above.
        self.core.state = if self.core.array_kind == ArrayKind::Signed {
            State::VarintSigned
        } else {
            State::VarintUnsigned
        };
        Ok(())
    }
}
