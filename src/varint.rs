//! Base-128 varint and ZigZag codecs (see the SofaBuffers documentation
//! §2.2 / §2.3: <https://github.com/sofa-buffers/documentation>).
//!
//! The decoder is incremental (one byte at a time) so it works across streaming
//! chunk boundaries. Its two state words live in [`crate::IStream`]'s `Core`
//! (which packs the decoder's own tail padding with the decoder-mode flags), so
//! this module holds only the value-width constant and the ZigZag helpers; the
//! encoder side is implemented inline in [`crate::ostream`] in terms of a
//! single-byte push.

use crate::{Signed, Unsigned};

/// Number of value bits; bounds the maximum varint length.
pub(crate) const VALUE_BITS: u8 = Unsigned::BITS as u8;

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
