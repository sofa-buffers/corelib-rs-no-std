//! `sofab::floats` — bit-pattern equality for float arrays, exercised directly.
//!
//! "Equal" means the same IEEE-754 bit pattern at every index and the same
//! length: `-0.0` differs from `+0.0`, and a NaN equals only a NaN with the
//! identical payload (generator#636, CORELIB_PLAN §4.6). The width-generic
//! cases are written once as a macro and instantiated for fp32 and fp64.

macro_rules! suite {
    ($modname:ident, $feat:literal, $f:ty, $bits:ty, $eq:path, $nan_alt:expr, $snan:expr) => {
        #[cfg(feature = $feat)]
        mod $modname {
            type F = $f;
            type B = $bits;

            fn eq(a: &[F], b: &[F]) -> bool {
                $eq(a, b)
            }

            /// The reference the helper must agree with: a plain bit loop.
            fn reference(a: &[F], b: &[F]) -> bool {
                if a.len() != b.len() {
                    return false;
                }
                for i in 0..a.len() {
                    if a[i].to_bits() != b[i].to_bits() {
                        return false;
                    }
                }
                true
            }

            fn nan_a() -> F {
                F::NAN
            }
            fn nan_b() -> F {
                F::from_bits($nan_alt)
            }
            fn snan() -> F {
                F::from_bits($snan)
            }

            #[test]
            fn empty_arrays_are_equal() {
                assert!(eq(&[], &[]));
            }

            #[test]
            fn one_element() {
                assert!(eq(&[1.5], &[1.5]));
                assert!(!eq(&[1.5], &[2.5]));
            }

            #[test]
            fn equal_arrays() {
                assert!(eq(&[0.0, 1.5, -2.25], &[0.0, 1.5, -2.25]));
            }

            #[test]
            fn array_against_itself() {
                let v: [F; 5] = [0.0, -0.0, nan_a(), F::INFINITY, 1.5];
                assert!(eq(&v, &v));
            }

            #[test]
            fn negative_zero_differs_from_positive_zero() {
                assert!(!eq(&[-0.0], &[0.0]));
                assert!(!eq(&[0.0], &[-0.0]));
                // First, middle and last index.
                assert!(!eq(&[-0.0, 1.5, 2.5], &[0.0, 1.5, 2.5]));
                assert!(!eq(&[1.5, -0.0, 2.5], &[1.5, 0.0, 2.5]));
                assert!(!eq(&[1.5, 2.5, -0.0], &[1.5, 2.5, 0.0]));
                // The motivating case: default [0.0, 1.5] vs field [-0.0, 1.5].
                assert!(!eq(&[-0.0, 1.5], &[0.0, 1.5][..]));
                // IEEE == would say these are equal; this must not.
                assert!(-0.0 == (0.0 as F));
            }

            #[test]
            fn same_nan_pattern_is_equal() {
                assert!(eq(&[nan_a()], &[nan_a()]));
                assert!(eq(&[1.0, nan_b(), 2.0], &[1.0, nan_b(), 2.0]));
                assert!(eq(&[snan()], &[snan()]));
                // IEEE == says a NaN is not equal to itself; this must.
                let n = nan_a();
                #[allow(clippy::eq_op)]
                {
                    assert!(n != n);
                }
            }

            #[test]
            fn nan_with_different_payload_differs() {
                assert_ne!(nan_a().to_bits(), nan_b().to_bits());
                assert!(nan_a().is_nan() && nan_b().is_nan());
                assert!(!eq(&[nan_a()], &[nan_b()]));
                assert!(!eq(&[nan_a()], &[snan()]));
                assert!(!eq(&[1.0, nan_a()], &[1.0, nan_b()]));
            }

            #[test]
            fn infinities() {
                assert!(eq(&[F::INFINITY], &[F::INFINITY]));
                assert!(eq(&[F::NEG_INFINITY], &[F::NEG_INFINITY]));
                assert!(!eq(&[F::INFINITY], &[F::NEG_INFINITY]));
                assert!(!eq(&[F::INFINITY], &[F::MAX]));
            }

            #[test]
            fn subnormals() {
                let tiny = F::from_bits(1 as B);
                let tiny2 = F::from_bits(2 as B);
                assert!(tiny > 0.0 && tiny < F::MIN_POSITIVE);
                assert!(eq(&[tiny], &[tiny]));
                assert!(!eq(&[tiny], &[tiny2]));
                assert!(!eq(&[tiny], &[0.0]));
                assert!(!eq(&[-tiny], &[tiny]));
            }

            #[test]
            fn length_mismatch_both_directions() {
                assert!(!eq(&[], &[0.0]));
                assert!(!eq(&[0.0], &[]));
                assert!(!eq(&[0.0, 1.5], &[0.0, 1.5, 0.0]));
                assert!(!eq(&[0.0, 1.5, 0.0], &[0.0, 1.5]));
                // A shared prefix is not enough.
                assert!(!eq(&[1.0, 2.0, 3.0], &[1.0, 2.0]));
            }

            #[test]
            fn long_arrays_with_one_difference() {
                for n in [63usize, 64, 65, 129, 1000] {
                    let base: Vec<F> = (0..n).map(|i| i as F * 0.5).collect();
                    assert!(eq(&base, &base.clone()));
                    for pos in [0, n / 2, n - 1] {
                        let mut other = base.clone();
                        other[pos] = F::from_bits(other[pos].to_bits() ^ 1);
                        assert!(!eq(&base, &other), "n={n} pos={pos}");
                        assert!(!eq(&other, &base), "n={n} pos={pos}");
                    }
                }
            }

            #[test]
            fn long_arrays_negative_zero_at_start_middle_end() {
                let n = 200;
                for pos in [0, n / 2, n - 1] {
                    let mut a = vec![0.0 as F; n];
                    let b = vec![0.0 as F; n];
                    assert!(eq(&a, &b));
                    a[pos] = -0.0;
                    assert!(!eq(&a, &b), "pos={pos}");
                    assert!(!eq(&b, &a), "pos={pos}");
                }
            }

            #[test]
            fn accepts_vec_array_and_constant_slices() {
                let v: Vec<F> = vec![0.0, 1.5];
                const DEFAULT: [F; 2] = [0.0, 1.5];
                assert!(eq(&v[..], &DEFAULT[..]));
                assert!(eq(&v, &DEFAULT));
            }

            #[test]
            fn does_not_mutate_its_inputs() {
                let a: [F; 3] = [-0.0, nan_b(), 1.5];
                let b = a;
                let _ = eq(&a, &b);
                for i in 0..3 {
                    assert_eq!(a[i].to_bits(), b[i].to_bits());
                }
            }

            #[test]
            fn pseudo_random_cross_check_against_a_reference_loop() {
                // xorshift64*, fixed seed: deterministic.
                let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
                let mut next = move || {
                    s ^= s >> 12;
                    s ^= s << 25;
                    s ^= s >> 27;
                    s.wrapping_mul(0x2545_F491_4F6C_DD1D)
                };
                // A pool rich in the awkward patterns.
                let pool: [F; 8] = [
                    0.0,
                    -0.0,
                    nan_a(),
                    nan_b(),
                    F::INFINITY,
                    F::NEG_INFINITY,
                    F::from_bits(1 as B),
                    1.5,
                ];
                let (mut equal, mut unequal) = (0, 0);
                for _ in 0..4000 {
                    let len = (next() % 6) as usize;
                    let a: Vec<F> = (0..len).map(|_| pool[(next() % 8) as usize]).collect();
                    let mut b = a.clone();
                    match next() % 4 {
                        0 => {}
                        1 if len > 0 => {
                            let i = (next() % len as u64) as usize;
                            b[i] = pool[(next() % 8) as usize];
                        }
                        2 => b.push(pool[(next() % 8) as usize]),
                        _ => {
                            let len2 = (next() % 6) as usize;
                            b = (0..len2).map(|_| pool[(next() % 8) as usize]).collect();
                        }
                    }
                    let got = eq(&a, &b);
                    assert_eq!(got, reference(&a, &b), "a={a:?} b={b:?}");
                    if got {
                        equal += 1;
                    } else {
                        unequal += 1;
                    }
                }
                assert!(equal > 100 && unequal > 100, "{equal}/{unequal}");
            }
        }
    };
}

suite!(
    f32_suite,
    "fixlen",
    f32,
    u32,
    sofab::floats::bits_equal_f32,
    0x7fc0_1234,
    0x7fa0_0001
);
suite!(
    f64_suite,
    "fp64",
    f64,
    u64,
    sofab::floats::bits_equal_f64,
    0x7ff8_0000_0000_1234,
    0x7ff4_0000_0000_0001
);
