use super::*;
use tairix_vdu::palette::default_256;

fn default_256_colours() -> Vec<Rgb> {
    (0..=u8::MAX)
        .map(|pixel| {
            let [r, g, b] = default_256(pixel);
            Rgb::new(r, g, b)
        })
        .collect()
}

/// Mode 12's default colours, its flashing ones in their first state.
fn sixteen() -> Vec<Rgb> {
    let eight = [0, 1, 2, 3, 4, 5, 6, 7].map(|i: u8| {
        let level = |bit: u8| if i & bit != 0 { 255 } else { 0 };
        Rgb::new(level(1), level(2), level(4))
    });
    eight.iter().chain(&eight).copied().collect()
}

#[test]
fn a_mix_takes_each_channel_in_256ths() {
    let (over, under) = (Rgb::new(255, 0, 0x80), Rgb::new(0, 255, 0x40));
    assert_eq!(mix(over, under, 0), over);
    assert_eq!(mix(over, under, 128), Rgb::new(127, 127, 0x60));
    assert_eq!(mix(over, under, 255), Rgb::new(0, 254, 0x40));
}

#[test]
fn a_table_onto_sixteen_colours_is_risc_os_5_s() {
    // Every 37th entry of the table RISC OS 5's BlendTable built for the
    // default 256-colour palette over mode 12's at half translucency: the
    // destination's colours outermost, each blend its last nearest.
    let table = blended(&default_256_colours(), &sixteen(), 128).expect("room");
    assert_eq!(table.len(), 4096);
    let sampled: Vec<u8> = (0..32).map(|i| table[i * 37]).collect();
    assert_eq!(
        sampled,
        [
            8, 8, 8, 8, 8, 8, 8, 9, 8, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11,
            11, 11, 11, 12, 12, 12, 12
        ]
    );
}

#[test]
fn equally_near_colours_take_the_last() {
    assert_eq!(nearest_last(&sixteen(), Rgb::new(255, 0, 0)), 9);
    assert_eq!(nearest_last(&sixteen(), Rgb::BLACK), 8);
}

/// The sixteen colours the reference machine was given: no two channels
/// alike, so a weighed match and a plain one part.
fn custom_sixteen() -> Vec<Rgb> {
    (0..16u32)
        .map(|i| {
            let [r, ..] = (i * 91 + 40).to_le_bytes();
            let [g, ..] = (i * 53 + 200).to_le_bytes();
            let [b, ..] = (i * 147 + 77).to_le_bytes();
            Rgb::new(r, g, b)
        })
        .collect()
}

#[test]
fn a_table_onto_a_custom_palette_matches_by_plain_squares() {
    // Entries of the tables RISC OS 5's BlendTable built for these colours
    // over themselves: every 7th at 128, every 11th at 80.
    let colours = custom_sixteen();
    let half = blended(&colours, &colours, 128).expect("room");
    let sampled: Vec<u8> = half.iter().step_by(7).copied().collect();
    assert_eq!(
        sampled,
        [
            0, 9, 14, 4, 9, 12, 7, 9, 14, 9, 6, 4, 4, 9, 7, 6, 9, 7, 12, 9, 11, 14, 9, 4, 9, 4, 11,
            6, 9, 11, 2, 4, 14, 12, 14, 4, 4
        ]
    );
    let lighter = blended(&colours, &colours, 80).expect("room");
    let sampled: Vec<u8> = lighter.iter().step_by(11).copied().collect();
    assert_eq!(
        sampled,
        [0, 11, 6, 4, 7, 12, 2, 13, 9, 12, 9, 9, 4, 15, 10, 5, 14, 11, 6, 15, 7, 12, 13, 13]
    );
}
