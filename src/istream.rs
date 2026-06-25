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
    #[cfg(feature = "fixlen")]
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// A chunk of a blob field. See [`Visitor::string`] for the chunking model.
    #[cfg(feature = "fixlen")]
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// Start of an array field with `count` elements of the given `kind`. The
    /// elements follow via the scalar / float callbacks with the same `id`.
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

    // fixlen context
    #[cfg(feature = "fixlen")]
    fixlen_type: FixlenType,
    #[cfg(feature = "fixlen")]
    fixlen_total: usize,
    #[cfg(feature = "fixlen")]
    fixlen_remaining: usize,
    #[cfg(feature = "fixlen")]
    acc: [u8; 8],
    #[cfg(feature = "fixlen")]
    acc_len: usize,

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
            #[cfg(feature = "fixlen")]
            fixlen_type: FixlenType::Fp32,
            #[cfg(feature = "fixlen")]
            fixlen_total: 0,
            #[cfg(feature = "fixlen")]
            fixlen_remaining: 0,
            #[cfg(feature = "fixlen")]
            acc: [0; 8],
            #[cfg(feature = "fixlen")]
            acc_len: 0,
            #[cfg(feature = "sequence")]
            depth: 0,
        }
    }

    /// Feed a chunk of encoded bytes, pushing decoded fields to `visitor`.
    ///
    /// Returns [`Error::InvalidMsg`] on malformed input. Decoding can continue
    /// across many `feed` calls; the decoder keeps all state internally.
    pub fn feed<V: Visitor>(&mut self, data: &[u8], visitor: &mut V) -> Result<()> {
        let mut i = 0;
        while i < data.len() {
            // Fast path: stream string/blob payloads in bulk rather than
            // one callback per byte.
            #[cfg(feature = "fixlen")]
            if self.state == State::FixlenRaw {
                let take = (data.len() - i).min(self.fixlen_remaining);
                let offset = self.fixlen_total - self.fixlen_remaining;
                let chunk = &data[i..i + take];
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
        Ok(())
    }

    fn step<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        match self.state {
            State::Idle => self.step_idle(byte, visitor),
            State::VarintUnsigned => self.step_varint_unsigned(byte, visitor),
            State::VarintSigned => self.step_varint_signed(byte, visitor),
            #[cfg(feature = "fixlen")]
            State::FixlenLen => self.step_fixlen_len(byte, visitor),
            #[cfg(feature = "fixlen")]
            State::FixlenVal => self.step_fixlen_val(byte, visitor),
            // FixlenRaw is fully handled in `feed`'s bulk path.
            #[cfg(feature = "fixlen")]
            State::FixlenRaw => Ok(()),
            #[cfg(feature = "array")]
            State::ArrayCount => self.step_array_count(byte, visitor),
        }
    }

    #[cfg_attr(not(feature = "sequence"), allow(unused_variables))]
    fn step_idle<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        let header = match self.varint.push(byte)? {
            Some(v) => v,
            None => return Ok(()),
        };

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
                self.array_kind = ArrayKind::Fixlen;
                self.state = State::ArrayCount;
            }

            #[cfg(feature = "sequence")]
            T_SEQUENCE_START => {
                if self.depth == u32::MAX {
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

    fn step_varint_unsigned<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        if let Some(value) = self.varint.push(byte)? {
            visitor.unsigned(self.id, value);
            self.advance_after_element();
        }
        Ok(())
    }

    fn step_varint_signed<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        if let Some(zz) = self.varint.push(byte)? {
            visitor.signed(self.id, zigzag_decode(zz));
            self.advance_after_element();
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
    fn step_fixlen_len<V: Visitor>(&mut self, byte: u8, _visitor: &mut V) -> Result<()> {
        let header = match self.varint.push(byte)? {
            Some(v) => v,
            None => return Ok(()),
        };

        let subtype = FixlenType::from_raw((header & 0x07) as u8)?;
        let length = (header >> 3) as usize;
        // Reject implausibly large fixlen lengths (matches SOFAB_FIXLEN_MAX).
        if header >> 3 > ARRAY_MAX {
            return Err(Error::InvalidMsg);
        }

        self.fixlen_type = subtype;
        self.fixlen_total = length;
        self.fixlen_remaining = length;
        self.acc_len = 0;

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
                // String/blob are not valid as fixlen-array elements.
                #[cfg(feature = "array")]
                if self.in_array {
                    return Err(Error::InvalidMsg);
                }
                if length == 0 {
                    match subtype {
                        FixlenType::Str => _visitor.string(self.id, 0, 0, &[]),
                        FixlenType::Blob => _visitor.blob(self.id, 0, 0, &[]),
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
        self.acc[self.acc_len] = byte;
        self.acc_len += 1;
        self.fixlen_remaining -= 1;
        if self.fixlen_remaining != 0 {
            return Ok(());
        }

        match self.fixlen_type {
            FixlenType::Fp32 => {
                let bytes: [u8; 4] = self.acc[..4].try_into().unwrap();
                visitor.fp32(self.id, f32::from_le_bytes(bytes));
            }
            #[cfg(feature = "fp64")]
            FixlenType::Fp64 => {
                let bytes: [u8; 8] = self.acc[..8].try_into().unwrap();
                visitor.fp64(self.id, f64::from_le_bytes(bytes));
            }
            _ => return Err(Error::InvalidMsg),
        }

        // Next array element (reuse the element size) or back to idle.
        #[cfg(feature = "array")]
        if self.in_array {
            self.array_remaining -= 1;
            if self.array_remaining > 0 {
                self.fixlen_remaining = self.fixlen_total;
                self.acc_len = 0;
                return Ok(());
            }
            self.in_array = false;
        }
        self.state = State::Idle;
        Ok(())
    }

    #[cfg(feature = "array")]
    fn step_array_count<V: Visitor>(&mut self, byte: u8, visitor: &mut V) -> Result<()> {
        let count = match self.varint.push(byte)? {
            Some(v) => v,
            None => return Ok(()),
        };

        if count == 0 || count > ARRAY_MAX {
            return Err(Error::InvalidMsg);
        }
        let count = count as usize;
        self.array_remaining = count;
        self.in_array = true;
        visitor.array_begin(self.id, self.array_kind, count);

        self.state = match self.array_kind {
            ArrayKind::Unsigned => State::VarintUnsigned,
            ArrayKind::Signed => State::VarintSigned,
            #[cfg(feature = "fixlen")]
            ArrayKind::Fixlen => State::FixlenLen,
        };
        Ok(())
    }
}
