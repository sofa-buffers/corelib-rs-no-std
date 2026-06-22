//! Base-128 varint and ZigZag codecs (see the SofaBuffers documentation
//! §2.2 / §2.3: <https://github.com/sofa-buffers/documentation>).
//!
//! The decoder is incremental (one byte at a time) so it works across streaming
//! chunk boundaries. The encoder side is implemented inline in [`crate::ostream`]
//! in terms of a single-byte push, so this module only holds the decode state
//! and the ZigZag helpers.

use crate::{Error, Result, Signed, Unsigned};

/// Number of value bits; bounds the maximum varint length.
const VALUE_BITS: u32 = Unsigned::BITS;

/// Incremental unsigned-varint decoder.
#[derive(Default)]
pub(crate) struct VarintDecoder {
    value: Unsigned,
    shift: u32,
}

impl VarintDecoder {
    pub(crate) const fn new() -> Self {
        Self { value: 0, shift: 0 }
    }

    /// Feed one byte.
    ///
    /// * `Ok(Some(v))` — a complete value was decoded (state auto-resets).
    /// * `Ok(None)` — more bytes are needed.
    /// * `Err(InvalidMsg)` — the varint is longer than the value type allows.
    pub(crate) fn push(&mut self, byte: u8) -> Result<Option<Unsigned>> {
        // OR in the 7 payload bits at the current position. Bits shifted beyond
        // the value width are discarded (matches the C reference).
        self.value |= ((byte & 0x7F) as Unsigned) << self.shift;
        self.shift += 7;

        if byte & 0x80 == 0 {
            let v = self.value;
            self.value = 0;
            self.shift = 0;
            return Ok(Some(v));
        }

        // Continuation bit set but no more room -> overflow.
        if self.shift >= VALUE_BITS {
            self.value = 0;
            self.shift = 0;
            return Err(Error::InvalidMsg);
        }

        Ok(None)
    }
}

/// ZigZag encode a signed value to its unsigned varint representation.
#[inline]
pub(crate) fn zigzag_encode(v: Signed) -> Unsigned {
    // `wrapping_shl` avoids the debug-mode overflow panic for `Signed::MIN`.
    (v.wrapping_shl(1) ^ (v >> (Signed::BITS - 1))) as Unsigned
}

/// ZigZag decode an unsigned varint back to a signed value.
#[inline]
pub(crate) fn zigzag_decode(u: Unsigned) -> Signed {
    ((u >> 1) as Signed) ^ -((u & 1) as Signed)
}
