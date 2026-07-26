use super::{decode_float16, encode_float16};

#[test]
fn float16_decodes_spec_vectors_and_special_values() {
    for (bits, expected) in [
        (0x0000, 0.0),
        (0x0001, 2_f64.powi(-24)),
        (0x03ff, 1023.0 * 2_f64.powi(-24)),
        (0x0400, 2_f64.powi(-14)),
        (0x3c00, 1.0),
        (0x4228, 3.078125),
        (0x4b4b, 14.5859375),
        (0xe040, -544.0),
        (0x7bff, 65_504.0),
        (0x7c00, f64::INFINITY),
        (0xfc00, f64::NEG_INFINITY),
    ] {
        assert_eq!(
            decode_float16(bits).to_bits(),
            expected.to_bits(),
            "{bits:#06x}"
        );
    }
    assert_eq!(decode_float16(0x8000).to_bits(), (-0.0_f64).to_bits());
    assert!(decode_float16(0x7e00).is_nan());
    assert!(decode_float16(0xfe00).is_nan());
}

#[test]
fn float16_encode_rounds_nearest_with_even_ties() {
    let half_subnormal = 2_f64.powi(-25);
    let min_subnormal = 2_f64.powi(-24);
    let one_ulp_at_one = 2_f64.powi(-10);

    assert_eq!(encode_float16(half_subnormal), 0x0000);
    assert_eq!(encode_float16(half_subnormal.next_up()), 0x0001);
    assert_eq!(encode_float16(min_subnormal), 0x0001);
    assert_eq!(encode_float16(1.0 + one_ulp_at_one / 2.0), 0x3c00);
    assert_eq!(encode_float16(1.0 + 3.0 * one_ulp_at_one / 2.0), 0x3c02);
    assert_eq!(encode_float16(65_519.0), 0x7bff);
    assert_eq!(encode_float16(65_520.0), 0x7c00);
}

#[test]
fn float16_encode_preserves_sign_and_canonicalizes_nan() {
    assert_eq!(encode_float16(0.0), 0x0000);
    assert_eq!(encode_float16(-0.0), 0x8000);
    assert_eq!(encode_float16(f64::INFINITY), 0x7c00);
    assert_eq!(encode_float16(f64::NEG_INFINITY), 0xfc00);
    assert_eq!(encode_float16(f64::NAN), 0x7e00);
    assert_eq!(encode_float16(-f64::NAN), 0xfe00);
}

#[test]
fn every_non_nan_float16_pattern_roundtrips() {
    for bits in u16::MIN..=u16::MAX {
        let exponent = bits & 0x7c00;
        let fraction = bits & 0x03ff;
        if exponent != 0x7c00 || fraction == 0 {
            assert_eq!(encode_float16(decode_float16(bits)), bits, "{bits:#06x}");
        }
    }
}
