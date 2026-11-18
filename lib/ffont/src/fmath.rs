// SPDX-License-Identifier: Apache-2.0
//! The float functions `core` lacks, for the magnitudes fonts use (|x| < 2^31).
//! Plain arithmetic only, so every machine gets the same bits.

pub fn floor(x: f32) -> f32 {
    let t = x as i32 as f32;
    if t > x { t - 1.0 } else { t }
}

pub fn ceil(x: f32) -> f32 {
    let t = x as i32 as f32;
    if t < x { t + 1.0 } else { t }
}

/// Rounds half away from zero.
pub fn round(x: f32) -> f32 {
    if x >= 0.0 { floor(x + 0.5) } else { -floor(-x + 0.5) }
}

/// The square root, by Newton's method from an exponent-halving first guess.
pub fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = f32::from_bits((x.to_bits() >> 1) + 0x1FC0_0000);
    for _ in 0..5 {
        g = 0.5 * (g + x / g);
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_std_on_ordinary_values() {
        for &x in &[-3.7f32, -3.0, -0.5, -0.2, 0.0, 0.2, 0.5, 2.5, 3.0, 1234.56] {
            assert_eq!(floor(x), x.floor(), "floor {x}");
            assert_eq!(ceil(x), x.ceil(), "ceil {x}");
            assert_eq!(round(x), x.round(), "round {x}");
        }
        for &x in &[0.25f32, 1.0, 2.0, 100.0, 12345.0] {
            assert!((sqrt(x) - x.sqrt()).abs() <= x.sqrt() * 1e-6, "sqrt {x}");
        }
    }
}
