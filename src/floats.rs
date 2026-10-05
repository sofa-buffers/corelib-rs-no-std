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
//!
//! Elements are compared by OR-ing together a per-element difference and
//! testing the accumulator once, not by a branch per element. That form
//! vectorises on targets without a 64-bit integer compare (baseline x86-64
//! SSE2 has `pxor`/`por` but no `pcmpeqq`), where a per-element compare of the
//! 64-bit patterns stays a scalar loop. An array of at most `SHORT` elements is
//! one such pass. A longer one is compared in blocks of `BLOCK` elements, so a
//! mismatch still stops early. A field that has left its default most often
//! differs at the start, so from `EARLY` elements on the first two elements are
//! tested on their own before the pass.

/// Longest array compared in a single pass.
#[cfg(any(feature = "fixlen", feature = "fp64"))]
const SHORT: usize = 16;

/// Elements per pass once an array is longer than `SHORT`.
#[cfg(any(feature = "fixlen", feature = "fp64"))]
const BLOCK: usize = 32;

/// Generates the private pass / block helpers and the public `bits_equal_*`
/// for one element width. `$step` folds one pair into the accumulator, giving
/// zero iff the pair has the same bit pattern. `$early` is the length from
/// which the first two elements are tested on their own.
#[cfg(any(feature = "fixlen", feature = "fp64"))]
macro_rules! bits_equal_impl {
    (
        $feat:literal, $f:ty, $bits:ty, $width:literal, $early:expr, $step:expr,
        $name:ident, $pass:ident, $long:ident
    ) => {
        /// Folds every pair of the two equal-length slices; zero iff all patterns match.
        #[cfg(feature = $feat)]
        #[inline(always)]
        fn $pass(a: &[$f], b: &[$f]) -> $bits {
            a.iter().zip(b).fold(0, $step)
        }

        /// The longer-than-`SHORT` path: block by block, stopping at the first
        /// block that differs. The slices must have the same length.
        #[cfg(feature = $feat)]
        #[inline(never)]
        fn $long(a: &[$f], b: &[$f]) -> bool {
            let (ca, cb) = (a.chunks_exact(BLOCK), b.chunks_exact(BLOCK));
            let (ra, rb) = (ca.remainder(), cb.remainder());
            for (x, y) in ca.zip(cb) {
                if $pass(x, y) != 0 {
                    return false;
                }
            }
            $pass(ra, rb) == 0
        }

        #[doc = concat!(
                    "`true` iff `a` and `b` have the same length and, at every index, the same\n",
                    $width, "-bit IEEE-754 bit pattern.\n\n",
                    "`+0.0` and `-0.0` differ; two NaNs are equal only when their bit patterns\n",
                    "(payload included) are identical."
                )]
        #[cfg(feature = $feat)]
        #[inline]
        pub fn $name(a: &[$f], b: &[$f]) -> bool {
            if a.len() != b.len() {
                return false;
            }
            if b.len() <= SHORT {
                (b.len() < $early || $pass(&a[..2], &b[..2]) == 0) && $pass(a, b) == 0
            } else {
                $pass(&a[..2], &b[..2]) == 0 && $long(a, b)
            }
        }
    };
}

#[cfg(feature = "fixlen")]
bits_equal_impl!(
    "fixlen",
    f32,
    u32,
    "32",
    16,
    |acc, (x, y)| acc | u32::from(x.to_bits() != y.to_bits()),
    bits_equal_f32,
    pass_f32,
    equal_long_f32
);
#[cfg(feature = "fp64")]
bits_equal_impl!(
    "fp64",
    f64,
    u64,
    "64",
    8,
    |acc, (x, y)| acc | (x.to_bits() ^ y.to_bits()),
    bits_equal_f64,
    pass_f64,
    equal_long_f64
);
