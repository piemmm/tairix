//! The sRGB transfer (IEC 61966-2-1): what an encoded channel means as light.

use tairix_util::mathf;

/// An sRGB-encoded channel in `0.0..=1.0` as the linear light it stands for.
#[must_use]
pub fn srgb_to_linear(encoded: f64) -> f64 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        mathf::exp(2.4 * mathf::ln((encoded + 0.055) / 1.055))
    }
}

/// Linear light in `0.0..=1.0` as its sRGB-encoded channel: the inverse of
/// [`srgb_to_linear`].
#[must_use]
pub fn linear_to_srgb(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * mathf::exp(mathf::ln(linear) / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::{linear_to_srgb, srgb_to_linear};

    #[test]
    fn the_srgb_transfer_meets_its_reference_points() {
        assert!(srgb_to_linear(0.0).abs() < 1e-15);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-12);
        assert!((srgb_to_linear(0.5) - 0.214_041_1).abs() < 1e-7);
        assert!((linear_to_srgb(0.5) - 0.735_356_9).abs() < 1e-7);
        assert!((linear_to_srgb(0.001) - 0.012_92).abs() < 1e-12);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-12);
    }

    /// The two pieces of each curve meet at its knee, and each direction
    /// undoes the other across the whole range.
    #[test]
    fn the_srgb_transfer_is_continuous_and_inverts() {
        assert!((0.040_45 / 12.92 - srgb_to_linear(0.040_450_001)).abs() < 1e-7);
        assert!((12.92 * 0.003_130_8 - linear_to_srgb(0.003_130_801)).abs() < 1e-6);
        for step in 0..=1000u32 {
            let encoded = f64::from(step) / 1000.0;
            assert!((linear_to_srgb(srgb_to_linear(encoded)) - encoded).abs() < 1e-9);
        }
    }
}
