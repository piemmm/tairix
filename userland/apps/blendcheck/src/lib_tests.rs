extern crate std;

use super::*;

use alloc::format;
use alloc::vec;

use tairix_abi::driver::display::DisplayFormat;

fn mode(width_px: u32, height_px: u32, format: DisplayFormat) -> DisplayMode {
    DisplayMode {
        width_px,
        height_px,
        stride_bytes: width_px * 4 + 16,
        format,
    }
}

const SCREEN: (u32, u32) = (1920, 1080);

fn screen() -> DisplayMode {
    mode(SCREEN.0, SCREEN.1, DisplayFormat::Bgra8888)
}

/// The pixel the frame holds at `(x, y)`, decoded.
fn shown(pattern: &Pattern, mode: &DisplayMode, x: u32, y: u32) -> Pixel {
    let at = (y * mode.stride_bytes + x * 4) as usize;
    let bytes: [u8; 4] = pattern.frame[at..at + 4].try_into().expect("four bytes");
    ChannelOrder::for_format(mode.format)
        .expect("an order")
        .decode(bytes)
}

#[test]
fn the_command_line_takes_the_help_switches_and_nothing_else() {
    assert_eq!(parse(&[]), Ok(Command::Run));
    for help in ["-h", "-?", "--help"] {
        assert_eq!(parse(&[help]), Ok(Command::Help), "{help}");
        assert_eq!(
            parse(&["now", help]),
            Ok(Command::Help),
            "{help} after an operand"
        );
    }
    assert_eq!(
        parse(&["now"]),
        Err(BlendcheckError::ExtraOperand(String::from("now")))
    );
    assert!(matches!(parse(&["-q"]), Err(BlendcheckError::Option(_))));
}

#[test]
fn the_panes_stack_down_the_screen_with_the_bare_ramp_around_each() {
    let output = screen();
    let pattern = pattern(&output).expect("a pattern");
    let mut below = 0;
    for pane in pattern.panes {
        assert_eq!((pane.x, pane.width_px), (0, SCREEN.0), "the whole ramp");
        assert!(pane.height_px > 0);
        assert!(pane.y > below, "bare ramp above every pane");
        below = pane.y + pane.height_px;
        assert!(below < SCREEN.1);
        for y in [pane.y - 1, below] {
            assert_eq!(
                shown(&pattern, &output, SCREEN.0 - 1, y),
                ramp(SCREEN.0 - 1, SCREEN.0),
                "row {y} beside the pane is the bare ramp"
            );
        }
    }
}

#[test]
fn the_desktop_panes_are_its_own_operators_and_the_engine_pane_is_left_bare() {
    let output = mode(97, 60, DisplayFormat::Rgba8888);
    let pattern = pattern(&output).expect("a pattern");
    let [dithered, rounded, engine] = pattern.panes;
    for x in 0..output.width_px {
        let under = ramp(x, output.width_px);
        for y in dithered.y..dithered.y + dithered.height_px {
            assert_eq!(
                shown(&pattern, &output, x, y),
                GLASS.over_biased(under, DitherRow::at(y).bias(x)),
                "dithered at ({x}, {y})"
            );
        }
        for y in rounded.y..rounded.y + rounded.height_px {
            assert_eq!(
                shown(&pattern, &output, x, y),
                GLASS.over(under),
                "rounded at ({x}, {y})"
            );
        }
        for y in engine.y..engine.y + engine.height_px {
            assert_eq!(shown(&pattern, &output, x, y), under, "bare at ({x}, {y})");
        }
    }
}

/// The longest run of equal pixels along row `y`.
fn plateau(pattern: &Pattern, mode: &DisplayMode, y: u32) -> u32 {
    let (mut longest, mut run) = (0, 0);
    let mut last = None;
    for x in 0..mode.width_px {
        let pixel = shown(pattern, mode, x, y);
        run = if last == Some(pixel) { run + 1 } else { 1 };
        longest = longest.max(run);
        last = Some(pixel);
    }
    longest
}

#[test]
fn the_rounded_pane_shows_the_plateaus_the_dither_breaks() {
    let output = screen();
    let pattern = pattern(&output).expect("a pattern");
    let [dithered, rounded, _] = pattern.panes;
    assert!(
        plateau(&pattern, &output, rounded.y) >= 40,
        "rounded to nearest, the glass leaves wide steps"
    );
    for y in dithered.y..dithered.y + 8 {
        assert!(
            plateau(&pattern, &output, y) < 40,
            "row {y} of the dithered pane keeps a step as wide as the rounded one"
        );
    }
    for x in (0..output.width_px).step_by(7) {
        let near = shown(&pattern, &output, x, rounded.y);
        let dither = shown(&pattern, &output, x, dithered.y);
        assert!(
            near.r.abs_diff(dither.r) <= 1,
            "the dither moves a pixel at most one level, at {x}"
        );
    }
}

#[test]
fn the_glass_is_laid_into_a_buffer_row_by_row_at_its_stride() {
    let output = mode(16, 30, DisplayFormat::Bgra8888);
    let pattern = pattern(&output).expect("a pattern");
    let pane = pattern.engine_pane();
    let stride = pane.width_px * 4 + 12;
    let mut buffer = vec![0xAAu8; (stride * pane.height_px) as usize];
    assert!(pattern.lay_glass(&mut buffer, stride));
    let order = ChannelOrder::for_format(output.format).expect("an order");
    let glass = order.encode(GLASS);
    for row in buffer.chunks(stride as usize) {
        let (pixels, padding) = row.split_at(pane.width_px as usize * 4);
        assert!(pixels.chunks(4).all(|pixel| pixel == glass));
        assert!(
            padding.iter().all(|&byte| byte == 0xAA),
            "padding untouched"
        );
    }
    assert!(
        !pattern.lay_glass(&mut buffer[1..], stride),
        "a short buffer"
    );
    assert!(
        !pattern.lay_glass(&mut buffer, pane.width_px * 4 - 4),
        "a stride narrower than a row"
    );
}

#[test]
fn the_engine_layer_shows_the_whole_glass_over_its_pane() {
    let pattern = pattern(&screen()).expect("a pattern");
    let pane = pattern.engine_pane();
    let buffer = BufferId::new(7).expect("an id");
    let layer = pattern.engine_layer(buffer);
    assert_eq!(layer.buffer, buffer);
    assert_eq!(
        (layer.dst_x, layer.dst_y),
        (pane.x.cast_signed(), pane.y.cast_signed())
    );
    assert_eq!(
        (layer.dst_width_px, layer.dst_height_px),
        (pane.width_px, pane.height_px)
    );
    assert_eq!(layer.opacity, u8::MAX);
    let caps = AccelCaps {
        max_layers: 2,
        max_width_px: SCREEN.0,
        max_height_px: SCREEN.1,
        per_layer_opacity: false,
        hw_scaling: false,
        band_free_blend: false,
    };
    assert!(pattern.fits(&caps));
    assert_eq!(layer.validate(&caps, pane.width_px, pane.height_px), Ok(()));
    let narrow = AccelCaps {
        max_width_px: SCREEN.0 - 1,
        ..caps
    };
    assert!(!pattern.fits(&narrow));
}

#[test]
fn a_screen_too_small_for_the_panes_has_no_pattern() {
    assert!(pattern(&mode(1, 60, DisplayFormat::Rgba8888)).is_none());
    assert!(pattern(&mode(64, 5, DisplayFormat::Rgba8888)).is_none());
    assert!(pattern(&mode(64, 6, DisplayFormat::Rgba8888)).is_some());
    let short_stride = DisplayMode {
        stride_bytes: 64 * 4 - 4,
        ..mode(64, 60, DisplayFormat::Rgba8888)
    };
    assert!(pattern(&short_stride).is_none());
}

/// Every locale's document names the switches the parser accepts: the
/// tokens are language-neutral, so each translation carries them as the
/// canonical one does. Read from the bundle's own on-disk `Help/` tree.
#[test]
fn help_documents_the_parser_switches() {
    let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = format!("{help_root}/{locale}/blendcheck.md");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
        assert!(
            text.contains("`-h, -?`"),
            "{locale}/blendcheck.md must document `-h, -?`"
        );
    }
}
