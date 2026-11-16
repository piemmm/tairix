//! The C64's sixteen colours, computed from the VIC-II's luminance levels
//! and colour angles as Philip Timmermans measured them, through YUV, with
//! the PAL picture's gamma carried to sRGB's.

use tairix_util::mathf;

/// Which revision's luminances: the first chips had five levels, later ones
/// nine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Luma {
    /// The first 6569s and 6567s.
    First,
    /// Every later revision.
    Later,
}

/// Each colour's luminance, black 0 and white 32.
const FIRST: [u8; 16] = [0, 32, 8, 24, 16, 16, 8, 24, 16, 8, 16, 8, 16, 24, 16, 24];
const LATER: [u8; 16] = [0, 32, 10, 20, 12, 16, 8, 24, 12, 8, 16, 10, 15, 24, 15, 20];

/// Each colour's angle in sixteenths of the colour wheel; the greys have
/// none.
const HUE: [Option<u8>; 16] = [
    None,
    None,
    Some(4),
    Some(12),
    Some(2),
    Some(10),
    Some(15),
    Some(7),
    Some(5),
    Some(6),
    Some(4),
    None,
    None,
    Some(10),
    Some(15),
    None,
];

/// The monitor's settings the measurements were taken at: brightness and
/// saturation at half, contrast full.
const BRIGHTNESS: f64 = 50.0;
const CONTRAST: f64 = 100.0;
const SATURATION: f64 = 50.0;

/// The PAL picture's gamma, and sRGB's.
const SOURCE_GAMMA: f64 = 2.8;
const TARGET_GAMMA: f64 = 2.2;

/// The sixteen colours as sRGB red, green and blue.
#[must_use]
pub fn palette(luma: Luma) -> [[u8; 3]; 16] {
    let levels = match luma {
        Luma::First => FIRST,
        Luma::Later => LATER,
    };
    let gain = CONTRAST / 100.0 + 0.2;
    core::array::from_fn(|colour| {
        let y = (8.0 * f64::from(levels[colour]) + BRIGHTNESS - 50.0) * gain;
        let (u, v) = HUE[colour].map_or((0.0, 0.0), |sector| {
            let angle = (11.25 + 22.5 * f64::from(sector)).to_radians();
            let radius = 0.8 * SATURATION * gain;
            (radius * mathf::cos(angle), radius * mathf::sin(angle))
        });
        [y + 1.140 * v, y - 0.396 * u - 0.581 * v, y + 2.029 * u].map(channel)
    })
}

/// One channel, clipped to the display's range and carried from PAL's
/// gamma to sRGB's.
fn channel(level: f64) -> u8 {
    let level = mathf::clamp(level, 0.0, 255.0) / 255.0;
    let corrected = 255.0 * mathf::power(level, SOURCE_GAMMA / TARGET_GAMMA);
    u8::try_from(mathf::round_i32(corrected)).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_and_white_are_the_display_s_ends_and_the_greys_are_grey() {
        for luma in [Luma::First, Luma::Later] {
            let colours = palette(luma);
            assert_eq!(colours[0], [0; 3]);
            assert_eq!(colours[1], [255; 3]);
            for grey in [11, 12, 15] {
                let [r, g, b] = colours[grey];
                assert!(r == g && g == b, "{luma:?} {grey}");
            }
        }
    }

    #[test]
    fn the_later_greys_climb_in_order() {
        let colours = palette(Luma::Later);
        assert!(colours[11][0] < colours[12][0] && colours[12][0] < colours[15][0]);
    }

    #[test]
    fn each_colour_leans_the_way_its_name_says() {
        let colours = palette(Luma::Later);
        let [r, g, b] = colours[2];
        assert!(r > g && r > b, "red {:?}", colours[2]);
        let [r, g, b] = colours[6];
        assert!(b > r && b > g, "blue {:?}", colours[6]);
        let [r, g, b] = colours[5];
        assert!(g > r && g > b, "green {:?}", colours[5]);
        let [r, g, b] = colours[7];
        assert!(r > b && g > b, "yellow {:?}", colours[7]);
        assert!(colours[10][0] > colours[2][0], "light red is lighter");
    }
}
