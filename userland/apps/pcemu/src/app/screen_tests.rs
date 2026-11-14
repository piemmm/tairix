use super::*;
use tairix_crt::screen::fit;
use tairix_raster::resample::Region;

#[test]
fn a_machine_window_opens_to_show_a_vga_screen_at_one_pixel_a_dot() {
    for bar in [0, 26, 40] {
        let (width, height) = opening_extent(bar);
        assert_eq!(
            fit(width, height - bar),
            Some(Region {
                x: 0,
                y: 0,
                width: 640,
                height: 480
            }),
            "over a {bar}-pixel bar"
        );
    }
}
