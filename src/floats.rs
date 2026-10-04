//! Bit-pattern equality for float arrays — the **support layer** beside the
//! codec, not the codec.
//!
//! Nothing here touches the wire. A generated encoder omits a field iff its
//! value equals its declared default (MESSAGE_SPEC §2), and floats round-trip
//! bit-for-bit (CORELIB_PLAN §4.6), so "equals" for a float array must mean
//! *the same IEEE-754 bit patterns*, not IEEE `==`: `[-0.0, 1.5]` is **not**
//! the default `[0.0, 1.5]`, and a NaN equals only a NaN with the identical
//! payload. Every generated backend used to compare with an elementwise `==`
//! and so dropped a `-0.0` element (generator#636).
//!
//! The comparison has the same shape for every schema — only the element width
//! differs — so it lives here once instead of being emitted into every
//! generated crate (generator#587, ARCHITECTURE §8). Generated code calls it
//! with the field array and a constant default, both as slices:
//!
//! ```
//! # #[cfg(feature = "fixlen")] {
//! let field: [f32; 2] = [-0.0, 1.5];
//! assert!(!sofab::floats::bits_equal_f32(&field[..], &[0.0, 1.5][..]));
//! # }
//! ```
//!
//! The length is compared first; then every element's bit pattern. Pure `core`:
//! no allocation, no mutation, no panic, no `unsafe`. A block compare
//! (`memcmp`) is deliberately not used: the crate is `#![forbid(unsafe_code)]`,
//! `core` has no safe slice-to-bytes view, and the unsafe variant measured
//! larger on thumbv6m.

/// `true` iff `a` and `b` have the same length and, at every index, the same
/// 32-bit IEEE-754 bit pattern.
///
/// `+0.0` and `-0.0` differ; two NaNs are equal only when their bit patterns
/// (payload included) are identical.
#[cfg(feature = "fixlen")]
#[inline]
pub fn bits_equal_f32(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// `true` iff `a` and `b` have the same length and, at every index, the same
/// 64-bit IEEE-754 bit pattern.
///
/// `+0.0` and `-0.0` differ; two NaNs are equal only when their bit patterns
/// (payload included) are identical.
#[cfg(feature = "fp64")]
#[inline]
pub fn bits_equal_f64(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}
