//! Shared types and wire constants (see the SofaBuffers documentation §2:
//! <https://github.com/sofa-buffers/documentation>).

/// SofaBuffers wire/API version implemented by this library.
///
/// Normative per the architecture spec (`API_VERSION == 1`); matches
/// `SOFAB_API_VERSION` in the C reference.
pub const API_VERSION: u32 = 1;

/// Field identifier type. Application-assigned; need not be contiguous.
pub type Id = u32;

/// Largest valid field id (`INT32_MAX`), matching `SOFAB_ID_MAX` in C.
pub const ID_MAX: Id = i32::MAX as u32;

/// Maximum nested-sequence depth (`MAX_DEPTH`, §4.9/§6.2), matching
/// `SOFAB_MAX_DEPTH` in C.
///
/// An encoder must not open more than this many nested sequences, and a decoder
/// rejects a message that nests deeper with [`crate::Error::InvalidMsg`] rather
/// than risk unbounded recursion / stack growth.
#[cfg(feature = "sequence")]
pub const MAX_DEPTH: u32 = 255;

/// Unsigned value type used by the scalar API.
///
/// The reference C library uses a 64-bit value type by default; this port
/// follows that so the wire format and varint lengths match exactly. Disabling
/// the (default-on) `value64` feature narrows it to 32 bits, which
/// removes all double-width arithmetic on 32-bit MCUs (the single largest
/// footprint item) at the cost of not being able to represent / decode values
/// above 2³²−1 (mirrors a 32-bit `sofab_value_t` build of the C library).
///
/// The value width controls a public type, so it is *not* additive. Application
/// code that depends on a specific width can guard it with
/// [`require!`](crate::require)`(value64)` / `require!(value32)` (see the crate
/// docs).
#[cfg(feature = "value64")]
pub type Unsigned = u64;
/// Signed value type used by the scalar API.
#[cfg(feature = "value64")]
pub type Signed = i64;

/// Unsigned value type used by the scalar API (32-bit build, `value64` off).
#[cfg(not(feature = "value64"))]
pub type Unsigned = u32;
/// Signed value type used by the scalar API (32-bit build, `value64` off).
#[cfg(not(feature = "value64"))]
pub type Signed = i32;

/// Maximum number of elements in an array / bytes in a fixlen field
/// (`INT32_MAX`), matching `SOFAB_ARRAY_MAX` / `SOFAB_FIXLEN_MAX` on 32/64-bit
/// `size_t` platforms.
#[cfg(any(feature = "array", feature = "fixlen"))]
pub(crate) const ARRAY_MAX: Unsigned = i32::MAX as Unsigned;

// --- 3-bit wire field type tags (low 3 bits of the field header varint) ------
pub(crate) const T_VARINT_UNSIGNED: u8 = 0x0;
pub(crate) const T_VARINT_SIGNED: u8 = 0x1;
#[cfg(feature = "fixlen")]
pub(crate) const T_FIXLEN: u8 = 0x2;
#[cfg(feature = "array")]
pub(crate) const T_VARINTARRAY_UNSIGNED: u8 = 0x3;
#[cfg(feature = "array")]
pub(crate) const T_VARINTARRAY_SIGNED: u8 = 0x4;
#[cfg(all(feature = "array", feature = "fixlen"))]
pub(crate) const T_FIXLENARRAY: u8 = 0x5;
#[cfg(feature = "sequence")]
pub(crate) const T_SEQUENCE_START: u8 = 0x6;
#[cfg(feature = "sequence")]
pub(crate) const T_SEQUENCE_END: u8 = 0x7;

/// Sub-type of a fixed-length field (the 3-bit tag inside the fixlen header).
#[cfg(feature = "fixlen")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FixlenType {
    /// 32-bit IEEE-754 float, little-endian on the wire.
    Fp32 = 0x0,
    /// 64-bit IEEE-754 double, little-endian on the wire.
    #[cfg(feature = "fp64")]
    Fp64 = 0x1,
    /// UTF-8 / raw text (no NUL on the wire).
    Str = 0x2,
    /// Arbitrary raw bytes.
    Blob = 0x3,
}

#[cfg(feature = "fixlen")]
impl FixlenType {
    /// Decode a 3-bit fixlen tag from the wire, rejecting unsupported subtypes.
    pub(crate) fn from_raw(raw: u8) -> crate::Result<Self> {
        match raw {
            0x0 => Ok(FixlenType::Fp32),
            #[cfg(feature = "fp64")]
            0x1 => Ok(FixlenType::Fp64),
            0x2 => Ok(FixlenType::Str),
            0x3 => Ok(FixlenType::Blob),
            _ => Err(crate::Error::InvalidMsg),
        }
    }
}

/// Element type of an array, reported to a [`crate::Visitor`] at the start of an
/// array field.
///
/// A fixlen array names its **element subtype** (`Fp32` / `Fp64`) rather than a
/// collapsed "fixlen" category, because a consumer has to compare it against a
/// declared element type before it may apply any schema bound: a header whose
/// subtype contradicts the schema is a field that is skipped whole
/// (CORELIB_PLAN §4.8 step 3, MESSAGE_SPEC §7.3), and a bound that belongs to a
/// different field must not be applied to it. That is only possible if the kind
/// distinguishes fp32 from fp64, and if the array is announced *after* the
/// `fixlen_word` that carries the subtype — see [`crate::Visitor::array_begin`].
///
/// The discriminants are normative and shared by every SofaBuffers corelib that
/// exposes this hook.
#[cfg(feature = "array")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ArrayKind {
    /// Unsigned-integer elements (delivered via [`crate::Visitor::unsigned`]).
    Unsigned = 0,
    /// Signed-integer elements (delivered via [`crate::Visitor::signed`]).
    Signed = 1,
    /// 32-bit float elements (delivered via [`crate::Visitor::fp32`]).
    #[cfg(feature = "fixlen")]
    Fp32 = 2,
    /// 64-bit float elements (delivered via `Visitor::fp64`).
    ///
    /// Present whenever `fixlen` is — the enum is a shared contract, so a
    /// consumer's `match` on it stays feature-independent — but never reported
    /// by a build without `fp64`, which rejects the fp64 subtype outright.
    #[cfg(feature = "fixlen")]
    Fp64 = 3,
}
