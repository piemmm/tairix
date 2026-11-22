//! The colour models ColourTrans converts to and from, each channel a 16.16
//! number, as RISC OS 5 works them: HSV with its hue in degrees, CMYK with
//! black taken equally from cyan, magenta and yellow, and CIE XYZ through
//! RISC OS 5's matrices. A register can hold anything, so the HSV and CMYK
//! sums wrap as the processor's do.

/// One, in 16.16.
const ONE: i32 = 1 << 16;
/// A turn and a sixth of one, in 16.16 degrees.
const TURN: i32 = 360 * ONE;
const SIXTH: i32 = 60 * ONE;
/// The hue given a colour with none.
pub(crate) const NO_HUE: i32 = -1;

/// `ColourTrans_ConvertRGBToHSV`: hue, saturation and value. The hue's
/// share of its sixth is rounded away from nought, as RISC OS 5 rounds it.
pub(crate) fn rgb_to_hsv([r, g, b]: [i32; 3]) -> [i32; 3] {
    let max = r.max(g).max(b);
    let span = max.wrapping_sub(r.min(g).min(b));
    let saturation = if max == 0 { 0 } else { ratio(span, max) };
    if span == 0 {
        return [NO_HUE, saturation, max];
    }
    let (sixths, along) = if r == max {
        (0, g.wrapping_sub(b))
    } else if g == max {
        (2, b.wrapping_sub(r))
    } else {
        (4, r.wrapping_sub(g))
    };
    let share = (u64::from(along.unsigned_abs()) << 16).div_ceil(u64::from(span.unsigned_abs()));
    let share = i32::try_from(share).unwrap_or(i32::MAX);
    let signed = if along < 0 {
        share.wrapping_neg()
    } else {
        share
    };
    let hue = (sixths * SIXTH).wrapping_add(signed.wrapping_mul(60));
    [
        if hue < 0 { hue.wrapping_add(TURN) } else { hue },
        saturation,
        max,
    ]
}

/// `ColourTrans_ConvertHSVToRGB`: none where hue and saturation are both
/// nought, which RISC OS 5 refuses.
pub(crate) fn hsv_to_rgb([hue, saturation, value]: [i32; 3]) -> Option<[i32; 3]> {
    if saturation == 0 {
        return (hue != 0).then_some([value; 3]);
    }
    let hue = hue.rem_euclid(TURN);
    let sixth = hue / SIXTH;
    let along = (hue - sixth * SIXTH) / 60;
    let lowest = times(value, ONE.wrapping_sub(saturation));
    let falling = times(value, ONE.wrapping_sub(times(saturation, along)));
    let rising = times(value, ONE.wrapping_sub(times(saturation, ONE - along)));
    Some(match sixth {
        0 => [value, rising, lowest],
        1 => [falling, value, lowest],
        2 => [lowest, value, rising],
        3 => [lowest, falling, value],
        4 => [rising, lowest, value],
        _ => [value, lowest, falling],
    })
}

/// `ColourTrans_ConvertRGBToCMYK`.
pub(crate) fn rgb_to_cmyk(rgb: [i32; 3]) -> [i32; 4] {
    let [cyan, magenta, yellow] = rgb.map(|channel| ONE.wrapping_sub(channel));
    let black = cyan.min(magenta).min(yellow);
    [
        cyan.wrapping_sub(black),
        magenta.wrapping_sub(black),
        yellow.wrapping_sub(black),
        black,
    ]
}

/// `ColourTrans_ConvertCMYKToRGB`: each channel what its ink and the black
/// leave, never below nought.
pub(crate) fn cmyk_to_rgb([cyan, magenta, yellow, black]: [i32; 4]) -> [i32; 3] {
    [cyan, magenta, yellow].map(|ink| ONE.wrapping_sub(ink.wrapping_add(black)).max(0))
}

/// RGB to CIE XYZ and back, each coefficient over `1 << MATRIX_SHIFT`, as
/// RISC OS 5 holds them.
const RGB_TO_CIE: [[i64; 3]; 3] = [
    [8_022_977, 5_008_999, 2_929_325],
    [4_420_302, 10_989_894, 1_367_018],
    [332_161, 2_691_402, 15_232_494],
];
const CIE_TO_RGB: [[i64; 3]; 3] = [
    [45_959_282, -19_205_029, -7_114_795],
    [-18_773_465, 34_032_595, 556_074],
    [2_314_857, -5_594_372, 18_535_481],
];
const MATRIX_SHIFT: u32 = 24;

/// `ColourTrans_ConvertRGBToCIE`: none where a channel overflows a
/// register.
pub(crate) fn rgb_to_cie(rgb: [i32; 3]) -> Option<[i32; 3]> {
    transform(&RGB_TO_CIE, rgb)
}

/// `ColourTrans_ConvertCIEToRGB`, likewise.
pub(crate) fn cie_to_rgb(xyz: [i32; 3]) -> Option<[i32; 3]> {
    transform(&CIE_TO_RGB, xyz)
}

/// `vector` through `matrix`, each row's sum cut toward minus infinity.
fn transform(matrix: &[[i64; 3]; 3], vector: [i32; 3]) -> Option<[i32; 3]> {
    let mut out = [0; 3];
    for (channel, row) in out.iter_mut().zip(matrix) {
        let sum: i64 = row.iter().zip(vector).map(|(&c, v)| c * i64::from(v)).sum();
        *channel = i32::try_from(sum >> MATRIX_SHIFT).ok()?;
    }
    Some(out)
}

/// `a` over `b` in 16.16, cut toward nought.
fn ratio(a: i32, b: i32) -> i32 {
    i32::try_from((i64::from(a) << 16) / i64::from(b)).unwrap_or(i32::MAX)
}

/// `a` times `b` in 16.16, cut toward minus infinity.
fn times(a: i32, b: i32) -> i32 {
    i32::try_from((i64::from(a) * i64::from(b)) >> 16).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests;
