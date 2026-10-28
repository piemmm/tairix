//! The layered present against an engine that composites what it is handed,
//! so each scene is held to the software composite of the same scene.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageRect, DisplayMode, DisplayPower,
};
use tairix_abi::DriverError;
use tairix_display::{Base, BaseFrame, CursorPlane, DrawInPlace, LayerPresenter, Presenter};

use crate::color::{Color, Pixel};
use crate::corner::Corners;
use crate::geometry::{Point, Rect, Region};
use crate::pointer::{Ghost, Halo, HaloRing};
use crate::surface::Surface;
use crate::tests::{
    decorated, mode, new_compositor, opaque, solid_cursor, unbudgeted_compositor, DrawnFrames,
    MockDisplay, SETTLED_NS,
};
use crate::window::Window;
use crate::{Compositor, Presentation, WindowFrame, WindowId, WindowSizeState};

use super::Source;

const BLUE: Color = Color::rgb(0, 0, 255);
const RED: Color = Color::rgb(255, 0, 0);
const GREEN: Color = Color::rgb(0, 255, 0);

struct Buffer {
    id: BufferId,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

/// What a mock cursor plane refuses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PlaneRefuses {
    Nothing,
    /// Every image it is handed.
    Images,
    /// Every move it is asked for.
    Moves,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PlaneOp {
    Show {
        width: u32,
        height: u32,
        hot: (u32, u32),
    },
    Move(i32, i32),
    Hide,
}

/// A layer engine that composites each stack it is handed over the frame,
/// premultiplied *over* at the scan-out's 8 bits, as a device does.
struct MockEngine {
    mode: DisplayMode,
    caps: AccelCaps,
    cursor: Option<CursorCaps>,
    ready: bool,
    next: u32,
    room: usize,
    buffers: Vec<Buffer>,
    frame: Vec<u8>,
    screen: Vec<u8>,
    stacks: Vec<(bool, Vec<AccelLayer>)>,
    /// The base's damage the last layered present carried.
    base_damage: Vec<DamageRect>,
    frames: usize,
    refusal: Option<DriverError>,
    /// Most layers over the base it composites at once.
    deepest: usize,
    /// Stacks it was asked to show, taken or not.
    stacks_asked: usize,
    /// Buffers it was asked for, given or not.
    allocs_asked: usize,

    plane: Vec<PlaneOp>,
    plane_up: bool,
    /// What the plane refuses.
    plane_refuses: PlaneRefuses,
    /// Images the plane was handed, taken or not.
    plane_asks: usize,
    /// How many frames had been shown when each move reached the plane.
    moved_after: Vec<usize>,
    /// The display's own frames, where a base may be drawn into them.
    drawn: Option<DrawnFrames>,
}

const CAPS: AccelCaps = AccelCaps {
    max_layers: 8,
    max_width_px: 1024,
    max_height_px: 1024,
    per_layer_opacity: false,
    hw_scaling: false,
    band_free_blend: false,
};

impl MockEngine {
    fn new(mode: DisplayMode) -> Self {
        let len = (mode.stride_bytes * mode.height_px) as usize;
        Self {
            mode,
            caps: CAPS,
            cursor: None,
            ready: true,
            next: 1,
            room: usize::MAX,
            buffers: Vec::new(),
            frame: vec![0; len],
            screen: vec![0; len],
            stacks: Vec::new(),
            base_damage: Vec::new(),
            frames: 0,
            refusal: None,
            deepest: usize::MAX,
            stacks_asked: 0,
            allocs_asked: 0,

            plane: Vec::new(),
            plane_up: false,
            plane_refuses: PlaneRefuses::Nothing,
            plane_asks: 0,
            moved_after: Vec::new(),
            drawn: Some(DrawnFrames::new(mode)),
        }
    }

    /// An engine holding no frame of its own, whose base is copied in from the
    /// compositor's.
    fn without_frames(mut self) -> Self {
        self.drawn = None;
        self
    }

    /// Frames shown so far, composited or stacked.
    fn shown(&self) -> usize {
        self.frames + self.stacks.len()
    }

    fn with_cursor(mut self) -> Self {
        self.cursor = Some(CursorCaps {
            max_width_px: 64,
            max_height_px: 64,
        });
        self
    }

    fn buffer(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.iter().find(|buffer| buffer.id == id)
    }

    /// The base's damage the last layered present carried, as a region.
    fn base_region(&self) -> Region {
        let mut region = Region::new();
        for rect in &self.base_damage {
            region.add(Rect::new(
                rect.x.cast_signed(),
                rect.y.cast_signed(),
                rect.width_px,
                rect.height_px,
            ));
        }
        region
    }

    /// The last stack, without the base.
    fn layers(&self) -> &[AccelLayer] {
        let (based, stack) = self.stacks.last().expect("a layered present");
        if *based {
            &stack[1..]
        } else {
            stack
        }
    }

    fn take(&mut self, damage: &[DamageRect], frame: &[u8]) {
        let stride = self.mode.stride_bytes as usize;
        for rect in damage {
            for y in rect.y..rect.y + rect.height_px {
                let at = y as usize * stride + rect.x as usize * 4;
                let len = rect.width_px as usize * 4;
                self.frame[at..at + len].copy_from_slice(&frame[at..at + len]);
            }
        }
    }

    fn lay(&mut self, layer: &AccelLayer) {
        let Some(buffer) = self.buffer(layer.buffer) else {
            panic!("a layer names a buffer the engine never allocated");
        };
        let (bw, bytes) = (buffer.width as usize, buffer.bytes.clone());
        let stride = self.mode.stride_bytes as usize;
        for ly in 0..layer.dst_height_px {
            for lx in 0..layer.dst_width_px {
                let x = i64::from(layer.dst_x) + i64::from(lx);
                let y = i64::from(layer.dst_y) + i64::from(ly);
                let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                    continue;
                };
                if x >= self.mode.width_px || y >= self.mode.height_px {
                    continue;
                }
                let sx = (layer.src.x + lx * layer.src.width_px / layer.dst_width_px) as usize;
                let sy = (layer.src.y + ly * layer.src.height_px / layer.dst_height_px) as usize;
                let src = bytes[(sy * bw + sx) * 4..(sy * bw + sx) * 4 + 4]
                    .iter()
                    .map(|&channel| faded(channel, layer.opacity));
                let at = y as usize * stride + x as usize * 4;
                let keep = 255 - u32::from(faded(bytes[(sy * bw + sx) * 4 + 3], layer.opacity));
                for (under, laid) in self.screen[at..at + 4].iter_mut().zip(src) {
                    let blended = u32::from(laid) + (u32::from(*under) * keep + 127) / 255;
                    *under = u8::try_from(blended.min(255)).expect("a channel");
                }
            }
        }
    }
}

impl Presenter for MockEngine {
    fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn ready(&self) -> bool {
        self.ready
    }

    fn present(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        DamageRect::validate_list(damage, &self.mode)?;
        self.take(damage, frame);
        self.screen = self.frame.clone();
        self.frames += 1;
        Ok(())
    }

    fn present_drawn(
        &mut self,
        damage: &Region,
        draw: &mut DrawInPlace<'_>,
    ) -> Result<(), DriverError> {
        let frames = self.drawn.as_mut().ok_or(DriverError::Unsupported)?;
        let before = frames.draws;
        frames.present(damage, draw)?;
        if frames.draws > before {
            self.frame = frames.screen();
            self.screen = self.frame.clone();
            self.frames += 1;
        }
        Ok(())
    }

    fn set_power(&mut self, _power: DisplayPower) -> Result<(), DriverError> {
        Ok(())
    }

    fn layers(&mut self) -> Option<&mut dyn LayerPresenter> {
        Some(self)
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorPlane> {
        if self.cursor.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl LayerPresenter for MockEngine {
    fn caps(&self) -> AccelCaps {
        self.caps
    }

    fn alloc(&mut self, width_px: u32, height_px: u32) -> Result<BufferId, DriverError> {
        self.allocs_asked += 1;
        if self.buffers.len() >= self.room {
            return Err(DriverError::NoSpace);
        }
        let id = BufferId::new(self.next).ok_or(DriverError::NoSpace)?;
        self.next += 1;
        self.buffers.push(Buffer {
            id,
            width: width_px,
            height: height_px,
            bytes: vec![0; (width_px * height_px * 4) as usize],
        });
        Ok(id)
    }

    fn free(&mut self, id: BufferId) -> Result<(), DriverError> {
        let at = self
            .buffers
            .iter()
            .position(|buffer| buffer.id == id)
            .ok_or(DriverError::NotFound)?;
        self.buffers.remove(at);
        Ok(())
    }

    fn holds(&self, id: BufferId) -> bool {
        self.buffer(id).is_some()
    }

    fn pixels(&mut self, id: BufferId) -> Option<(&mut [u8], u32)> {
        self.buffers
            .iter_mut()
            .find(|buffer| buffer.id == id)
            .map(|buffer| (buffer.bytes.as_mut_slice(), buffer.width * 4))
    }

    fn present_layers(
        &mut self,
        base: Option<Base<'_, '_>>,
        layers: &[AccelLayer],
    ) -> Result<(), DriverError> {
        self.stacks_asked += 1;
        if let Some(refusal) = self.refusal {
            return Err(refusal);
        }
        if layers.len() > self.deepest {
            return Err(DriverError::OutOfRange);
        }
        assert!(
            self.ready,
            "a present while the last one waits for its latch"
        );
        for layer in layers {
            let buffer = self
                .buffer(layer.buffer)
                .expect("a buffer the engine allocated");
            layer.validate(&self.caps, buffer.width, buffer.height)?;
        }
        self.base_damage.clear();
        let opacity = base.as_ref().map(|base| base.opacity);
        if opacity.is_some_and(|opacity| opacity != u8::MAX && !self.caps.per_layer_opacity) {
            return Err(DriverError::Unsupported);
        }
        match base.map(|base| base.frame) {
            Some(BaseFrame::Copied { frame, damage }) => {
                self.base_damage.extend_from_slice(damage);
                self.take(damage, frame);
                self.screen = self.frame.clone();
            }
            Some(BaseFrame::Drawn { damage, draw }) => {
                let frames = self.drawn.as_mut().ok_or(DriverError::Unsupported)?;
                frames.base(damage, draw)?;
                self.base_damage
                    .extend(damage.rects().iter().map(|rect| DamageRect {
                        x: rect.left().cast_unsigned(),
                        y: rect.top().cast_unsigned(),
                        width_px: rect.width,
                        height_px: rect.height,
                    }));
                self.frame = frames.screen();
                self.screen = self.frame.clone();
            }
            None => {
                for pixel in self.screen.as_chunks_mut::<4>().0 {
                    *pixel = [0, 0, 0, 255];
                }
            }
        }
        // Over the engine's black, which the screen is opaque at.
        for pixel in self.screen.as_chunks_mut::<4>().0 {
            let opacity = opacity.unwrap_or(u8::MAX);
            *pixel = [
                faded(pixel[0], opacity),
                faded(pixel[1], opacity),
                faded(pixel[2], opacity),
                255,
            ];
        }
        for layer in layers {
            self.lay(layer);
        }
        let mut stack = Vec::new();
        if let Some(opacity) = opacity {
            stack.push(AccelLayer {
                buffer: BufferId::new(u32::MAX).expect("a marker"),
                src: DamageRect::full(&self.mode),
                dst_x: 0,
                dst_y: 0,
                dst_width_px: self.mode.width_px,
                dst_height_px: self.mode.height_px,
                opacity,
            });
        }
        stack.extend_from_slice(layers);
        self.stacks.push((opacity.is_some(), stack));
        Ok(())
    }
}

impl CursorPlane for MockEngine {
    fn caps(&self) -> CursorCaps {
        self.cursor.unwrap_or(CursorCaps {
            max_width_px: 0,
            max_height_px: 0,
        })
    }

    fn show(
        &mut self,
        image: &[u8],
        width_px: u32,
        height_px: u32,
        stride_bytes: u32,
        hot_x: u32,
        hot_y: u32,
    ) -> Result<(), DriverError> {
        assert!(image.len() >= (stride_bytes * height_px) as usize);
        self.plane_asks += 1;
        if self.plane_refuses == PlaneRefuses::Images {
            return Err(DriverError::NoSpace);
        }
        self.plane.push(PlaneOp::Show {
            width: width_px,
            height: height_px,
            hot: (hot_x, hot_y),
        });
        self.plane_up = true;
        Ok(())
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        if self.plane_refuses == PlaneRefuses::Moves {
            return Err(DriverError::DeviceFault);
        }
        self.plane.push(PlaneOp::Move(x, y));
        self.moved_after.push(self.shown());
        Ok(())
    }

    fn hide(&mut self) -> Result<(), DriverError> {
        self.plane.push(PlaneOp::Hide);
        self.plane_up = false;
        Ok(())
    }

    fn showing(&self) -> bool {
        self.plane_up
    }
}

/// A premultiplied channel at `opacity`, rounded to nearest as the software
/// reveal rounds.
fn faded(channel: u8, opacity: u8) -> u8 {
    u8::try_from((u32::from(channel) * u32::from(opacity) + 127) / 255).expect("a channel")
}

/// The scene `build` makes, composited in software: what a layered present
/// of it must show.
fn software(
    output: DisplayMode,
    background: Color,
    build: impl FnOnce(&mut Compositor),
) -> Vec<u8> {
    let mut c = new_compositor(output, background).expect("compositor");
    build(&mut c);
    let mut display = MockDisplay::new(output);
    c.present(&mut display).expect("presents");
    c.frame().to_vec()
}

/// Every channel of `shown` within `slack` of `expected`: an engine blends
/// without the dither the software composite rounds with.
fn assert_close(shown: &[u8], expected: &[u8], slack: u8) {
    assert_eq!(shown.len(), expected.len());
    for (at, (a, b)) in shown.iter().zip(expected).enumerate() {
        assert!(
            a.abs_diff(*b) <= slack,
            "byte {at} (pixel {}): engine {a}, software {b}",
            at / 4
        );
    }
}

fn pixel(bytes: &[u8], output: DisplayMode, x: u32, y: u32) -> [u8; 4] {
    let at = (y * output.stride_bytes + x * 4) as usize;
    [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
}

fn layer_pixel(engine: &MockEngine, layer: &AccelLayer, x: u32, y: u32) -> [u8; 4] {
    let buffer = engine.buffer(layer.buffer).expect("the layer's buffer");
    let at = ((y * buffer.width + x) * 4) as usize;
    [
        buffer.bytes[at],
        buffer.bytes[at + 1],
        buffer.bytes[at + 2],
        buffer.bytes[at + 3],
    ]
}

/// Three windows across a 32×24 screen.
fn three(c: &mut Compositor) -> [WindowId; 3] {
    [
        c.add_window(Point::new(1, 1), opaque(10, 10, RED)),
        c.add_window(Point::new(6, 4), opaque(10, 10, GREEN)),
        c.add_window(Point::new(14, 8), opaque(10, 10, Color::rgb(200, 200, 0))),
    ]
}

#[test]
fn the_top_windows_go_on_layers_over_a_base_holding_the_rest() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [_, middle, top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.caps.max_layers = 3;
    c.present(&mut engine).expect("presents");

    let layers = engine.layers();
    assert_eq!(layers.len(), 2, "the base and the two windows above");
    assert_eq!((layers[0].dst_x, layers[0].dst_y), (6, 4));
    assert_eq!((layers[1].dst_x, layers[1].dst_y), (14, 8));
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    assert!(c.on_display(middle) && c.on_display(top));
    assert_eq!(
        pixel(&engine.frame, output, 7, 5),
        [255, 0, 0, 255],
        "the base holds the window below the layers, and not the one above it"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );
}

#[test]
fn moving_a_window_on_a_free_layer_composites_and_bakes_nothing() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let before = *engine.layers().last().expect("the top layer");

    assert!(c.move_window(top, Point::new(20, 12)));
    assert!(c.has_damage(), "the layer owes its new place");
    c.present(&mut engine).expect("presents");

    let after = *engine.layers().last().expect("the top layer");
    assert_eq!((after.dst_x, after.dst_y), (20, 12));
    assert_eq!(
        after.buffer, before.buffer,
        "the buffer shown is named again, unbaked"
    );
    assert_eq!(c.frame_stats().damaged_px, 0, "nothing was composited");
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., top] = three(c);
            c.move_window(top, Point::new(20, 12));
        }),
        0,
    );
}

#[test]
fn a_repaint_is_baked_into_the_buffer_the_engine_is_not_showing() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let shown = engine.layers().last().expect("the top layer").buffer;

    let repaint = |c: &mut Compositor, id: WindowId| {
        let mut area = Region::new();
        area.add(Rect::new(2, 2, 3, 3));
        assert!(c.repaint_window(id, (10, 10), &area, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    Color::rgb(9, 9, 9),
                );
            }
        }));
    };
    repaint(&mut c, top);
    c.present(&mut engine).expect("presents");

    let baked = *engine.layers().last().expect("the top layer");
    assert_ne!(baked.buffer, shown, "the shown buffer is never written");
    assert_eq!(layer_pixel(&engine, &baked, 3, 3), [9, 9, 9, 255]);
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., top] = three(c);
            repaint(c, top);
        }),
        0,
    );
}

/// A repaint scattered across a window on a free layer stales the layer's own
/// pixels as one region, each rectangle made the layer's and clipped to it:
/// the screen shows exactly what the software composite does.
#[test]
fn a_scattered_repaint_reaches_a_free_layer_as_one_region() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    let repaint = |c: &mut Compositor, id: WindowId| {
        let mut area = Region::new();
        for rect in [
            Rect::new(0, 0, 2, 2),
            Rect::new(5, 1, 1, 4),
            Rect::new(3, 7, 4, 1),
            Rect::new(8, 8, 9, 9),
        ] {
            area.add(rect);
        }
        assert!(c.repaint_window(id, (10, 10), &area, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    Color::rgb(9, 9, 9),
                );
            }
        }));
    };
    repaint(&mut c, top);
    c.present(&mut engine).expect("presents");
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., top] = three(c);
            repaint(c, top);
        }),
        0,
    );
}

#[test]
fn an_idle_layered_scene_presents_nothing() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(
        !c.has_damage(),
        "a layered present consumes what it carried"
    );
    c.present(&mut engine).expect("an idle wake");
    assert_eq!(engine.stacks.len(), 1, "nothing more was handed over");
}

#[test]
fn a_translucent_window_is_flattened_over_what_lies_beneath_it() {
    let output = mode(32, 24);
    let build = |c: &mut Compositor| {
        let [.., top] = three(c);
        assert!(c.set_opacity(top, 160));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    let top = *engine
        .layers()
        .last()
        .expect("the translucent window's layer");
    for y in 0..top.src.height_px {
        for x in 0..top.src.width_px {
            assert_eq!(
                layer_pixel(&engine, &top, x, y)[3],
                255,
                "flattened layers are opaque"
            );
        }
    }
    assert_close(&engine.screen, &software(output, BLUE, build), 1);
}

/// A base drawn straight into the display's own frames shows what one composed
/// into the compositor's own frame and copied in does, beneath a frost, a
/// flattened window and a free one, through moves and a repaint.
#[test]
fn a_base_drawn_in_place_shows_what_a_copied_one_does() {
    let output = mode(48, 28);
    let build = |c: &mut Compositor| {
        let glass = rounded_frost(c, Point::new(10, 6));
        let veil = c.add_window(Point::new(30, 14), opaque(10, 8, RED));
        assert!(c.set_opacity(veil, 160));
        let top = c.add_window(Point::new(4, 18), opaque(6, 6, GREEN));
        [glass, veil, top]
    };
    let mut drawn = new_compositor(output, BLUE).expect("compositor");
    let mut copied = new_compositor(output, BLUE).expect("compositor");
    let [glass, veil, top] = build(&mut drawn);
    assert_eq!(build(&mut copied), [glass, veil, top]);
    let mut in_place = MockEngine::new(output);
    let mut copying = MockEngine::new(output).without_frames();
    let steps: [&dyn Fn(&mut Compositor) -> bool; 5] = [
        &|_| true,
        &|c| c.move_window(glass, Point::new(14, 9)),
        &|c| c.move_window(veil, Point::new(26, 12)),
        &|c| c.set_background(GREEN),
        &|c| c.move_window(top, Point::new(20, 2)),
    ];
    let mut bases = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        assert!(step(&mut drawn) && step(&mut copied));
        drawn.present(&mut in_place).expect("drawn in place");
        copied.present(&mut copying).expect("copied in");
        assert_eq!(drawn.presentation(), Some(Presentation::Layered));
        assert_eq!(copied.presentation(), Some(Presentation::Layered));
        assert_eq!(in_place.screen, copying.screen, "step {at}");
        bases.push(in_place.drawn.as_ref().map_or(0, |frames| frames.draws));
    }
    assert_eq!(
        bases,
        [1, 2, 3, 4, 4],
        "each base that changed drawn in place; a free window moving changes \
         nothing the base shows"
    );
}

/// A frame whose base has nothing new — a free window moving over it — draws
/// no base and encodes nothing, however far the compositor's own frame has
/// fallen behind the display's.
#[test]
fn a_frame_with_nothing_new_in_its_base_encodes_nothing() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("the first frame");
    assert!(c.move_window(top, Point::new(16, 10)));
    c.present(&mut engine).expect("the free window moved");
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    let stats = c.frame_stats();
    assert_eq!(stats.encoded_px, 0, "nothing was encoded: {stats:?}");
    assert_eq!(
        engine.drawn.as_ref().map_or(0, |frames| frames.draws),
        1,
        "the base on screen is named again"
    );
}

/// A flattened layer's changed pixels are encoded into the buffer it shows
/// next by the pass that composes them, so its bake has none of them left to
/// encode from the back buffer.
#[test]
fn a_flattened_layer_is_encoded_by_the_pass_that_composes_it() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    assert!(c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    let paint = |c: &mut Compositor, colour: Color| {
        c.present_window_content(top, 10, 10, |surface, changed| {
            surface.set(1, 4, colour.premultiply());
            changed.add(Rect::new(1, 4, 1, 1));
        })
    };
    c.present(&mut engine).expect("the first frame");
    // Both buffers are brought level, each then lacking only what changed
    // while the other was shown.
    assert_eq!(paint(&mut c, GREEN), Some(()));
    c.present(&mut engine).expect("the other buffer baked");
    assert_eq!(paint(&mut c, BLUE), Some(()));
    let mut encoded = Region::new();
    let planned = c.plan_composite(&mut encoded);
    c.compose_zones(&planned, Some(&mut engine));
    let layer = c
        .layers
        .as_ref()
        .and_then(|stack| {
            stack
                .layers
                .iter()
                .find(|layer| layer.window() == Some(top))
        })
        .expect("the flattened layer");
    let next = layer.target().expect("a buffer to show next");
    assert!(
        layer.held[next].stale.is_empty(),
        "the composed pixel is already in the buffer shown next"
    );
    assert!(
        layer.held[1 - next].stale.contains(Point::new(1, 4)),
        "and owed to the buffer on screen"
    );
}

#[test]
fn a_free_window_moving_under_a_flattened_one_rebakes_it_where_it_passes() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., middle, top] = three(&mut c);
    assert!(c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    assert!(c.move_window(middle, Point::new(12, 10)));
    c.present(&mut engine).expect("presents");
    assert!(
        c.frame_stats().damaged_px > 0,
        "the flattened footprint the move crossed"
    );
    assert!(
        c.frame_stats().damaged_px < 2 * 10 * 10,
        "and nothing of the move outside it: {}",
        c.frame_stats().damaged_px
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., middle, top] = three(c);
            c.set_opacity(top, 160);
            c.move_window(middle, Point::new(12, 10));
        }),
        1,
    );
}

/// The screen rectangle `layer` is shown at.
fn shown_at(layer: &AccelLayer) -> Rect {
    Rect::new(
        layer.dst_x,
        layer.dst_y,
        layer.dst_width_px,
        layer.dst_height_px,
    )
}

#[test]
fn a_dragged_flattened_window_hands_over_only_the_base_it_uncovered() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    assert!(c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    assert!(c.move_window(top, Point::new(16, 9)));
    c.present(&mut engine).expect("presents");

    let mut uncovered = Region::new();
    uncovered.add(Rect::new(14, 8, 10, 10));
    uncovered.subtract(Rect::new(16, 9, 10, 10));
    assert_eq!(
        shown_at(engine.layers().last().expect("the flattened layer")),
        Rect::new(16, 9, 10, 10)
    );
    assert_eq!(
        engine.base_region(),
        uncovered,
        "the base the window left, and none of what its layer covers"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., top] = three(c);
            c.set_opacity(top, 160);
            c.move_window(top, Point::new(16, 9));
        }),
        1,
    );
}

#[test]
fn a_frosted_window_dragged_off_where_it_lay_leaves_no_base_stale() {
    let output = mode(32, 20);
    let build = |c: &mut Compositor| {
        c.add_window(Point::ORIGIN, opaque(14, 14, RED));
        let glass = c.add_window(Point::new(4, 2), clear(12, 8));
        assert!(c.set_backdrop_blur(glass, 2));
        glass
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    for to in [
        Point::new(6, 3),
        Point::new(18, 10),
        Point::new(19, 11),
        Point::new(3, 1),
    ] {
        assert!(c.move_window(glass, to));
        c.present(&mut engine).expect("presents");
        let (frost, _) = frost_and_pixels(&engine);
        let covered = shown_at(&frost);
        assert!(
            !engine.base_region().intersects(covered),
            "at {to:?} the base named pixels its frost's layer covers"
        );
        assert_close(
            &engine.screen,
            &software(output, BLUE, |c| {
                let glass = build(c);
                c.move_window(glass, to);
            }),
            1,
        );
    }
}

#[test]
fn a_change_beneath_a_flattened_window_reaches_its_layer_and_not_the_base() {
    let output = mode(32, 24);
    let repaint = |c: &mut Compositor, id: WindowId| {
        let mut area = Region::new();
        area.add(Rect::new(8, 4, 2, 6));
        assert!(c.repaint_window(id, (10, 10), &area, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    Color::rgb(9, 9, 9),
                );
            }
        }));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [_, middle, top] = three(&mut c);
    assert!(c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    repaint(&mut c, middle);
    c.present(&mut engine).expect("presents");

    assert!(
        engine.base_region().is_empty(),
        "every pixel the repaint reached is under the flattened layer"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [_, middle, top] = three(c);
            c.set_opacity(top, 160);
            repaint(c, middle);
        }),
        1,
    );
}

#[test]
fn a_flattened_window_closed_shows_the_base_it_covered() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    assert!(c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(c.move_window(top, Point::new(15, 9)));
    c.present(&mut engine).expect("presents");

    assert!(c.remove(top));
    c.present(&mut engine).expect("presents");
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [.., top] = three(c);
            c.remove(top);
        }),
        0,
    );
}

/// A fully transparent surface, which frosted shows nothing but its frost.
fn clear(w: u32, h: u32) -> Surface {
    Surface::filled(w, h, Pixel::TRANSPARENT).expect("surface allocates")
}

/// Dark glass, translucent across its whole surface, as a frosted panel is.
fn glass(w: u32, h: u32) -> Surface {
    Surface::filled(w, h, Color::rgba(20, 24, 30, 0xD0).premultiply()).expect("surface")
}

/// A frosted window over an opaque one, the glass's id answered.
fn frosted(c: &mut Compositor) -> WindowId {
    c.add_window(Point::ORIGIN, opaque(12, 12, RED));
    let glass = c.add_window(Point::new(4, 2), glass(16, 8));
    assert!(c.set_backdrop_blur(glass, 2));
    glass
}

/// The frost layer and the free layer above it a frosted window is laid on,
/// at the top of the last stack.
fn frost_and_pixels(engine: &MockEngine) -> (AccelLayer, AccelLayer) {
    let layers = engine.layers();
    let [.., frost, pixels] = layers else {
        panic!("a frosted window takes two layers: {layers:?}");
    };
    (*frost, *pixels)
}

#[test]
fn a_frosted_window_is_its_frost_beneath_its_own_pixels() {
    let output = mode(24, 12);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    frosted(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Layered));

    let (frost, pixels) = frost_and_pixels(&engine);
    assert_eq!(shown_at(&frost), Rect::new(4, 2, 16, 8));
    assert_eq!(shown_at(&pixels), Rect::new(4, 2, 16, 8));
    for y in 0..frost.src.height_px {
        for x in 0..frost.src.width_px {
            assert_eq!(
                layer_pixel(&engine, &frost, x, y)[3],
                255,
                "a frost is opaque"
            );
            assert_eq!(
                layer_pixel(&engine, &pixels, x, y)[3],
                0xD0,
                "the glass as it is, for the engine to blend over its frost"
            );
        }
    }
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            frosted(c);
        }),
        1,
    );
}

#[test]
fn a_dragged_frosted_window_composes_its_frost_and_never_its_pixels() {
    let output = mode(32, 20);
    let drag = |engine: &mut MockEngine| {
        let mut c = new_compositor(output, BLUE).expect("compositor");
        let glass = frosted(&mut c);
        c.present(engine).expect("presents");
        assert!(c.move_window(glass, Point::new(7, 4)));
        c.present(engine).expect("presents");
        c.frame_stats()
    };
    let mut flat = MockEngine::new(output);
    flat.caps.max_layers = 2;
    let flattened = drag(&mut flat);
    assert_eq!(
        flat.layers().len(),
        1,
        "one layer, so the glass is flattened"
    );

    let mut engine = MockEngine::new(output);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = frosted(&mut c);
    c.present(&mut engine).expect("presents");
    let (frost_before, pixels_before) = frost_and_pixels(&engine);
    assert!(c.move_window(glass, Point::new(7, 4)));
    c.present(&mut engine).expect("presents");
    let split = c.frame_stats();
    let (frost, pixels) = frost_and_pixels(&engine);

    assert_eq!(
        pixels.buffer, pixels_before.buffer,
        "its own pixels only moved"
    );
    assert_eq!(shown_at(&pixels), Rect::new(7, 4, 16, 8));
    assert_ne!(
        frost.buffer, frost_before.buffer,
        "its frost was baked again"
    );
    assert_eq!(shown_at(&frost), Rect::new(7, 4, 16, 8));
    assert!(
        split.blended_px + 16 * 8 <= flattened.blended_px,
        "the glass is blended by the engine, not composited: {} against {}",
        split.blended_px,
        flattened.blended_px
    );
    assert!(split.blur_px > 0, "the first move blurs its field's margin");
    assert!(c.move_window(glass, Point::new(8, 5)));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.frame_stats().blur_px,
        0,
        "a move inside the field blurs nothing"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let glass = frosted(c);
            c.move_window(glass, Point::new(8, 5));
        }),
        1,
    );
}

#[test]
fn a_frosted_windows_own_change_reaches_its_pixels_and_not_its_frost() {
    let output = mode(24, 12);
    let repaint = |c: &mut Compositor, id: WindowId| {
        let mut area = Region::new();
        area.add(Rect::new(2, 2, 6, 3));
        assert!(c.repaint_window(id, (16, 8), &area, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    Color::rgb(240, 240, 240),
                );
            }
        }));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = frosted(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let (frost_before, _) = frost_and_pixels(&engine);

    repaint(&mut c, glass);
    c.present(&mut engine).expect("presents");
    let (frost, _) = frost_and_pixels(&engine);
    assert_eq!(c.frame_stats().damaged_px, 0, "nothing is composited");
    assert_eq!(frost.buffer, frost_before.buffer, "its frost is as it was");
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let glass = frosted(c);
            repaint(c, glass);
        }),
        1,
    );
}

#[test]
fn a_change_beneath_a_frosted_window_reaches_its_frost() {
    let output = mode(24, 12);
    let build = |c: &mut Compositor| {
        let under = c.add_window(Point::ORIGIN, opaque(12, 12, RED));
        let glass = c.add_window(Point::new(4, 2), glass(16, 8));
        assert!(c.set_backdrop_blur(glass, 2));
        under
    };
    let repaint = |c: &mut Compositor, id: WindowId| {
        let mut area = Region::new();
        area.add(Rect::new(6, 3, 4, 4));
        assert!(c.repaint_window(id, (12, 12), &area, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    GREEN,
                );
            }
        }));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let under = build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let (frost_before, pixels_before) = frost_and_pixels(&engine);

    repaint(&mut c, under);
    c.present(&mut engine).expect("presents");
    let (frost, pixels) = frost_and_pixels(&engine);
    assert_ne!(
        frost.buffer, frost_before.buffer,
        "the frost reads what changed"
    );
    assert_eq!(
        pixels.buffer, pixels_before.buffer,
        "the glass's own pixels did not"
    );
    assert!(c.frame_stats().blur_px > 0);
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let under = build(c);
            repaint(c, under);
        }),
        1,
    );
}

/// A rounded frosted window over stripes at `at`, the glass's id answered: its
/// corners mix its shape over what lies beneath them, which is the part of a
/// frost layer baked from the frame rather than the field.
fn rounded_frost(c: &mut Compositor, at: Point) -> WindowId {
    let mut stripes = Surface::filled(48, 28, GREEN.premultiply()).expect("surface");
    for x in (0..48).step_by(4) {
        stripes.fill_rect(x, 0, 2, 28, RED);
    }
    c.add_window(Point::ORIGIN, stripes);
    let glass = c.add_window(at, glass(20, 12));
    assert!(c.set_corners(glass, Corners::Rounded { radius: 4 }));
    assert!(c.set_backdrop_blur(glass, 4));
    glass
}

/// What a rounded glass's arc may differ by: its free layer is baked already
/// weighted by its coverage, and the engine blends that without the dither,
/// two roundings where the software composite takes one.
const ARC_SLACK: u8 = 2;

/// What `engine`'s buffers hold, buffer by buffer.
fn snapshot(engine: &MockEngine) -> Vec<(BufferId, Vec<u8>)> {
    engine
        .buffers
        .iter()
        .map(|buffer| (buffer.id, buffer.bytes.clone()))
        .collect()
}

/// The pixels, by buffer and buffer coordinate, that differ from `before`.
fn written(engine: &MockEngine, before: &[(BufferId, Vec<u8>)]) -> Vec<(BufferId, u32, u32)> {
    let mut written = Vec::new();
    for (id, was) in before {
        let Some(buffer) = engine.buffer(*id) else {
            continue;
        };
        for (at, (now, then)) in buffer.bytes.chunks(4).zip(was.chunks(4)).enumerate() {
            if now != then {
                let at = u32::try_from(at).expect("a small buffer");
                written.push((*id, at % buffer.width, at / buffer.width));
            }
        }
    }
    written
}

#[test]
fn a_frost_layer_holds_its_field_and_shows_its_window_of_it() {
    let output = mode(48, 28);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = rounded_frost(&mut c, Point::new(10, 6));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(c.move_window(glass, Point::new(11, 7)));
    c.present(&mut engine).expect("presents");

    let (frost, _) = frost_and_pixels(&engine);
    let buffer = engine.buffer(frost.buffer).expect("the frost's buffer");
    assert_eq!(
        (buffer.width, buffer.height),
        (20 + 2 * 4, 12 + 2 * 4),
        "its buffers hold the field: the window and a margin of the blur's radius"
    );
    assert_eq!(shown_at(&frost), Rect::new(11, 7, 20, 12));
    assert_eq!(
        (
            frost.src.x,
            frost.src.y,
            frost.src.width_px,
            frost.src.height_px
        ),
        (4, 4, 20, 12),
        "the engine shows the window's rectangle of it"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let glass = rounded_frost(c, Point::new(10, 6));
            c.move_window(glass, Point::new(11, 7));
        }),
        ARC_SLACK,
    );
}

#[test]
fn a_drag_inside_the_field_writes_the_frost_layer_only_at_its_corners() {
    let output = mode(48, 28);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = rounded_frost(&mut c, Point::new(10, 6));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    // The first move gives the field its margin, about the window at (11, 6);
    // it and the move after it bake both of the new layer's buffers whole.
    for x in [11, 12] {
        assert!(c.move_window(glass, Point::new(x, 6)));
        c.present(&mut engine).expect("presents");
    }
    let field_origin = Point::new(7, 2);
    let corners = |x: i32| {
        [(x, 6), (x + 16, 6), (x, 14), (x + 16, 14)]
            .map(|(cx, cy)| Rect::new(cx - field_origin.x, cy - field_origin.y, 4, 4))
    };
    // One way only: a buffer that last held the window where it returns to is
    // already right, and is rightly not written.
    for (from, to) in [(12, 13), (13, 14), (14, 15)] {
        let before = snapshot(&engine);
        assert!(c.move_window(glass, Point::new(to, 6)));
        c.present(&mut engine).expect("presents");
        let stats = c.frame_stats();
        assert_eq!(stats.blur_px, 0, "a move inside the field blurs nothing");
        let changed = written(&engine, &before);
        assert!(!changed.is_empty(), "the corners moved");
        let (frost, _) = frost_and_pixels(&engine);
        let reached: Vec<Rect> = [from - 1, from, to].into_iter().flat_map(corners).collect();
        for &(id, x, y) in &changed {
            assert_eq!(id, frost.buffer, "only the frost's buffer was written");
            let point = Point::new(x.cast_signed(), y.cast_signed());
            assert!(
                reached.iter().any(|corner| corner.contains(point)),
                "({x}, {y}) of the frost's buffer lies at no corner it left or reached"
            );
        }
        assert!(
            stats.encoded_px <= 2 * 12,
            "the frame encoded {} pixels: the strip the move uncovered, and no zone",
            stats.encoded_px
        );
        assert_close(
            &engine.screen,
            &software(output, BLUE, |c| {
                let glass = rounded_frost(c, Point::new(10, 6));
                c.move_window(glass, Point::new(11, 6));
                c.move_window(glass, Point::new(to, 6));
            }),
            ARC_SLACK,
        );
    }
}

#[test]
fn crossing_the_margin_rewrites_the_buffer_the_engine_is_not_showing() {
    let output = mode(48, 28);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = rounded_frost(&mut c, Point::new(10, 6));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(c.move_window(glass, Point::new(11, 6)));
    c.present(&mut engine).expect("presents");
    let (shown, _) = frost_and_pixels(&engine);
    let size = engine.buffer(shown.buffer).map(|b| (b.width, b.height));

    assert!(c.move_window(glass, Point::new(16, 6)));
    c.present(&mut engine).expect("presents");
    let (frost, _) = frost_and_pixels(&engine);
    assert_ne!(
        frost.buffer, shown.buffer,
        "the hidden buffer took the shifted field"
    );
    assert_eq!(
        engine.buffer(frost.buffer).map(|b| (b.width, b.height)),
        size,
        "the field shifted in place, the size it was"
    );
    assert_eq!(
        (frost.src.x, frost.src.y),
        (4, 4),
        "about the window where it lies"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let glass = rounded_frost(c, Point::new(10, 6));
            c.move_window(glass, Point::new(16, 6));
        }),
        ARC_SLACK,
    );
}

#[test]
fn a_frosted_window_dragged_off_the_left_edge_and_back_keeps_every_column() {
    // Its translucent pixels are repainted whole while it hangs past the
    // screen's left edge and only at their right-hand end once back, so a
    // buffer baked out there is the one every other repaint shows.
    let output = mode(96, 64);
    let build = |c: &mut Compositor| {
        let mut stripes = Surface::filled(96, 64, GREEN.premultiply()).expect("surface");
        for x in (0..96).step_by(6) {
            stripes.fill_rect(x, 0, 3, 64, RED);
        }
        c.add_window(Point::ORIGIN, stripes);
        let glass = c.add_window(Point::new(30, 20), glass(40, 24));
        assert!(c.set_window_frame(glass, WindowFrame::new(decorated())));
        assert!(c.set_corners(glass, Corners::Rounded { radius: 4 }));
        assert!(c.set_casts_shadow(glass, true));
        assert!(c.set_backdrop_blur(glass, 4));
        glass
    };
    let repaint = |c: &mut Compositor, id: WindowId, area: Rect, shade: u8| {
        let mut region = Region::new();
        region.add(area);
        assert!(c.repaint_window(id, (40, 24), &region, |surface, rects| {
            for rect in rects {
                surface.fill_rect(
                    rect.left().cast_unsigned(),
                    rect.top().cast_unsigned(),
                    rect.width,
                    rect.height,
                    Color::rgba(shade, 200, 255 - shade, 0xC0),
                );
            }
        }));
    };
    let mut layered = new_compositor(output, BLUE).expect("compositor");
    let mut soft = new_compositor(output, BLUE).expect("compositor");
    let glass = build(&mut layered);
    assert_eq!(build(&mut soft), glass);
    let mut engine = MockEngine::new(output);
    let mut display = MockDisplay::new(output);
    let mut step = 0u8;
    let out: Vec<i32> = (-44..=30).rev().step_by(7).collect();
    for round in 0..3 {
        for &x in out.iter().chain(out.iter().rev()) {
            step = step.wrapping_add(1);
            let shade = step.wrapping_mul(37);
            for c in [&mut layered, &mut soft] {
                assert!(c.move_window(glass, Point::new(x, 20)));
                if x < 0 && step.is_multiple_of(3) {
                    repaint(c, glass, Rect::new(0, 0, 40, 24), shade);
                } else if step.is_multiple_of(2) {
                    repaint(c, glass, Rect::new(30, 2, 10, 20), shade);
                }
            }
            layered.present(&mut engine).expect("layered");
            soft.present(&mut display).expect("composited");
            assert!(
                engine.layers().len() >= 2,
                "round {round} at x {x}: the glass rides its layers"
            );
            assert_close(&engine.screen, soft.frame(), ARC_SLACK);
        }
    }
}

#[test]
fn a_frosted_window_without_a_retained_blur_is_flattened() {
    let output = mode(24, 12);
    let mut c = unbudgeted_compositor(output);
    let glass = frosted(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let bounds = c.window(glass).expect("glass").bounds();
    let glass_layers: Vec<&AccelLayer> = engine
        .layers()
        .iter()
        .filter(|layer| shown_at(layer) == bounds)
        .collect();
    assert_eq!(
        glass_layers.len(),
        1,
        "one layer, its frost composited into it"
    );
    assert_eq!(
        layer_pixel(&engine, glass_layers[0], 4, 4)[3],
        255,
        "flattened, so opaque"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            frosted(c);
        }),
        1,
    );
}

#[test]
fn no_composite_zone_is_encoded_into_the_frame() {
    let output = mode(48, 28);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = rounded_frost(&mut c, Point::new(10, 6));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let zone = c.window(glass).expect("glass").bounds();
    let frame_within = |engine: &MockEngine| {
        let mut bytes = Vec::new();
        for y in zone.top()..zone.bottom() {
            for x in zone.left()..zone.right() {
                bytes.push(pixel(
                    &engine.frame,
                    output,
                    x.cast_unsigned(),
                    y.cast_unsigned(),
                ));
            }
        }
        bytes
    };
    let before = frame_within(&engine);
    assert!(c.set_background(GREEN));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        frame_within(&engine),
        before,
        "the frame under a frost layer is the base's, which never shows it"
    );
}

#[test]
fn a_window_moving_beside_a_frost_layer_reaches_its_frost() {
    // A frost samples past its window's edges, so a window on a layer of its
    // own that comes within the frost's reach of it — without touching its
    // rectangle — changes that frost all the same.
    let output = mode(40, 20);
    let build = |c: &mut Compositor| {
        c.add_window(Point::ORIGIN, opaque(40, 20, GREEN));
        let mover = c.add_window(Point::new(2, 6), opaque(4, 4, RED));
        let glass = c.add_window(Point::new(14, 4), glass(16, 10));
        assert!(c.set_backdrop_blur(glass, 3));
        mover
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let mover = build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let (frost_before, _) = frost_and_pixels(&engine);

    assert!(c.move_window(mover, Point::new(10, 6)));
    c.present(&mut engine).expect("presents");
    let (frost, _) = frost_and_pixels(&engine);
    assert_ne!(
        frost.buffer, frost_before.buffer,
        "the frost reads the window beside it"
    );
    assert!(c.frame_stats().blur_px > 0);
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let mover = build(c);
            c.move_window(mover, Point::new(10, 6));
        }),
        1,
    );
}

#[test]
fn a_frosted_window_given_one_layer_is_flattened_with_its_frost() {
    let output = mode(24, 12);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    frosted(&mut c);
    let mut engine = MockEngine::new(output);
    engine.caps.max_layers = 2;
    c.present(&mut engine).expect("presents");
    let layers = engine.layers();
    assert_eq!(layers.len(), 1);
    assert_eq!(
        layer_pixel(&engine, &layers[0], 0, 0)[3],
        255,
        "composited opaque"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            frosted(c);
        }),
        1,
    );
}

#[test]
fn a_frosted_window_refused_its_frosts_buffers_is_flattened_and_stays_so() {
    let output = mode(24, 12);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = frosted(&mut c);
    let mut engine = MockEngine::new(output);
    engine.room = 2;
    c.present(&mut engine).expect("presents");
    assert!(
        engine.stacks.is_empty(),
        "its pixels were given buffers, its frost was not, so neither is shown"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            frosted(c);
        }),
        1,
    );

    for x in [5, 4, 5] {
        assert!(c.move_window(glass, Point::new(x, 2)));
        c.present(&mut engine).expect("presents");
        let layers = engine.layers();
        assert_eq!(
            layers.len(),
            1,
            "flattened, its one layer, the window under it in the base"
        );
        assert_eq!(layer_pixel(&engine, &layers[0], 0, 0)[3], 255);
        assert_close(
            &engine.screen,
            &software(output, BLUE, |c| {
                let glass = frosted(c);
                c.move_window(glass, Point::new(x, 2));
            }),
            1,
        );
    }
    assert_eq!(
        engine.allocs_asked,
        2 + 1 + 2 + 1,
        "after one refusal a frame asks for nothing it was refused"
    );
}

#[test]
fn a_frosted_window_off_screen_keeps_the_windows_beneath_it_on_layers() {
    let output = mode(32, 20);
    let build = |c: &mut Compositor| {
        c.add_window(Point::new(2, 2), opaque(12, 12, RED));
        let glass = c.add_window(Point::new(-40, 2), glass(16, 8));
        assert!(c.set_backdrop_blur(glass, 2));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let layers = engine.layers();
    assert_eq!(
        layers.len(),
        2,
        "the window beneath and the glass's own pixels"
    );
    assert_eq!(shown_at(&layers[0]), Rect::new(2, 2, 12, 12));
    assert_close(&engine.screen, &software(output, BLUE, build), 1);
}

#[test]
fn a_frost_crossing_a_frosted_base_window_takes_its_whole_window_to_the_base() {
    let output = mode(32, 24);
    let build = |c: &mut Compositor| {
        let below = c.add_window(Point::new(2, 2), glass(20, 12));
        assert!(c.set_backdrop_blur(below, 2));
        let above = c.add_window(Point::new(16, 8), glass(10, 10));
        assert!(c.set_backdrop_blur(above, 2));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(
        engine.stacks.is_empty(),
        "neither window's pixels go on a layer without their frost"
    );
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    assert_close(&engine.screen, &software(output, BLUE, build), 0);
}

#[test]
fn of_two_overlapping_flattened_windows_the_lower_joins_the_base() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [_, middle, top] = three(&mut c);
    assert!(c.set_opacity(middle, 160) && c.set_opacity(top, 160));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let layers = engine.layers();
    assert_eq!(layers.len(), 1, "only the top flattened window has a layer");
    assert_eq!((layers[0].dst_x, layers[0].dst_y), (14, 8));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [_, middle, top] = three(c);
            c.set_opacity(middle, 160);
            c.set_opacity(top, 160);
        }),
        1,
    );
}

#[test]
fn a_flattened_footprint_crossing_a_frosted_base_window_joins_the_base() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let glass = c.add_window(Point::new(2, 2), clear(20, 12));
    assert!(c.set_backdrop_blur(glass, 2));
    let sheer = c.add_window(Point::new(16, 8), opaque(10, 10, RED));
    assert!(c.set_opacity(sheer, 160));
    let mut engine = MockEngine::new(output);
    engine.caps.max_layers = 2;
    c.present(&mut engine).expect("presents");
    assert!(engine.stacks.is_empty(), "no stack cuts the frost");
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    assert_eq!(engine.frames, 1);
}

#[test]
fn partially_transparent_content_rides_a_free_layer_where_the_engine_bands() {
    let output = mode(32, 24);
    let build = |c: &mut Compositor| {
        let veil =
            Surface::filled(8, 8, Color::rgba(255, 0, 0, 128).premultiply()).expect("surface");
        c.add_window(Point::new(4, 4), veil)
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = build(&mut c);
    let mut engine = MockEngine::new(output);
    assert!(!engine.caps.band_free_blend);
    c.present(&mut engine).expect("presents");
    let free = *engine.layers().last().expect("its layer");
    assert_eq!(
        layer_pixel(&engine, &free, 0, 0)[3],
        128,
        "the content as it is, for the engine to blend"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            build(c);
        }),
        1,
    );

    assert!(c.move_window(id, Point::new(10, 10)));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.frame_stats().damaged_px,
        0,
        "a free layer moves without a composite"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let id = build(c);
            c.move_window(id, Point::new(10, 10));
        }),
        1,
    );

    let mut area = Region::new();
    area.add(Rect::new(0, 0, 8, 8));
    assert!(c.repaint_window(id, (8, 8), &area, |surface, _| {
        surface.fill_rect(0, 0, 8, 8, RED);
    }));
    c.present(&mut engine).expect("presents");
    let repainted = *engine.layers().last().expect("its layer");
    assert_eq!(layer_pixel(&engine, &repainted, 0, 0)[3], 255);
    assert_eq!(
        c.frame_stats().damaged_px,
        0,
        "painted opaque, it keeps its free layer and the base is untouched"
    );
}

/// What an engine states of its blend is shown to the user and decides no
/// layer: the same scene is laid out and baked alike either way.
#[test]
fn what_an_engine_states_of_its_blend_decides_no_layer() {
    let output = mode(32, 24);
    let build = |c: &mut Compositor| {
        c.add_window(Point::ORIGIN, opaque(12, 12, RED));
        let glass = c.add_window(Point::new(4, 2), clear(16, 8));
        assert!(c.set_backdrop_blur(glass, 2));
        let veil =
            Surface::filled(8, 8, Color::rgba(0, 255, 0, 96).premultiply()).expect("surface");
        c.add_window(Point::new(22, 14), veil);
    };
    let present = |band_free_blend: bool| {
        let mut c = new_compositor(output, BLUE).expect("compositor");
        build(&mut c);
        let mut engine = MockEngine::new(output);
        engine.caps.band_free_blend = band_free_blend;
        c.present(&mut engine).expect("presents");
        let alphas: Vec<u8> = engine
            .layers()
            .iter()
            .map(|layer| layer_pixel(&engine, layer, 0, 0)[3])
            .collect();
        (engine.layers().to_vec(), alphas, engine.screen.clone())
    };
    let banded = present(false);
    assert_eq!(
        banded.1,
        [255, 255, 0, 96],
        "opaque, the frosted window's frost and its clear pixels, the veil free"
    );
    assert_eq!(present(true), banded);
}

#[test]
fn a_window_too_large_for_the_engine_stays_in_the_base() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(1, 1), opaque(10, 10, RED));
    c.add_window(Point::new(2, 2), opaque(20, 20, GREEN));
    let mut engine = MockEngine::new(output);
    engine.caps.max_width_px = 16;
    c.present(&mut engine).expect("presents");
    assert!(
        engine.stacks.is_empty(),
        "the top window cannot be a layer, so none below it is"
    );
    assert_eq!(engine.frames, 1);
}

#[test]
fn a_display_that_refuses_buffers_is_presented_the_frame() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.room = 1;
    c.present(&mut engine).expect("presents");
    assert!(engine.stacks.is_empty());
    assert!(
        engine.buffers.is_empty(),
        "the buffer it could have was given back"
    );
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );
}

#[test]
fn the_pointer_rides_the_cursor_plane_and_its_moves_present_nothing() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(28, 2));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    assert_eq!(
        engine.plane,
        [
            PlaneOp::Show {
                width: 4,
                height: 4,
                hot: (0, 0)
            },
            PlaneOp::Move(28, 2)
        ]
    );
    assert_eq!(
        pixel(&engine.screen, output, 29, 3),
        [0, 0, 255, 255],
        "the frame draws no pointer"
    );

    assert!(c.move_cursor(Point::new(30, 20)));
    assert!(!c.has_damage(), "a pointer on the plane damages nothing");
    c.place_pointer(&mut engine);
    assert_eq!(engine.plane.last(), Some(&PlaneOp::Move(30, 20)));
    assert_eq!(engine.stacks.len(), 1, "and presents nothing");
}

/// What a pointer carrying a window sees: with no move between frames, the
/// frame showing the window where it went is handed over before the plane
/// goes there, so the two reach the screen together.
#[test]
fn a_frame_s_present_moves_the_plane_once_the_frame_is_handed_over() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let carried = c.add_window(Point::new(2, 2), opaque(8, 8, GREEN));
    c.set_cursor(solid_cursor(4, RED), Point::new(4, 4));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    let before = engine.shown();

    assert!(c.move_window(carried, Point::new(12, 10)));
    assert!(c.move_cursor(Point::new(14, 12)));
    c.present(&mut engine).expect("presents");
    assert!(engine.shown() > before, "the frame went");
    assert_eq!(engine.plane.last(), Some(&PlaneOp::Move(14, 12)));
    assert_eq!(
        engine.moved_after.last(),
        Some(&engine.shown()),
        "the plane went after the frame did"
    );
}

/// A pointer that changes shape as it starts to carry a window shows its new
/// image where the old one stood, and goes where it is due only once the frame
/// carrying the window is handed over.
#[test]
fn a_new_shape_shows_where_the_old_stood_and_moves_after_the_frame() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let carried = c.add_window(Point::new(2, 2), opaque(8, 8, GREEN));
    c.set_cursor(solid_cursor(4, RED), Point::new(4, 4));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");

    assert!(c.move_window(carried, Point::new(12, 10)));
    c.set_cursor(solid_cursor(6, RED), Point::new(14, 12));
    c.present(&mut engine).expect("presents");
    let shown = engine
        .plane
        .iter()
        .rposition(|op| matches!(op, PlaneOp::Show { width: 6, .. }))
        .expect("the new shape was shown");
    assert_eq!(
        engine.plane[shown + 1..],
        [PlaneOp::Move(4, 4), PlaneOp::Move(14, 12)],
        "shown where the old one stood, then moved"
    );
    assert_eq!(
        engine.moved_after.last(),
        Some(&engine.shown()),
        "the plane went after the frame did"
    );
}

/// A pointer with nothing to draw — a gesture pinned against the screen's
/// edge — still moves at once rather than waiting for a frame that never
/// comes.
#[test]
fn a_present_with_nothing_to_draw_still_moves_the_plane() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(4, 4));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    let shown = engine.shown();
    assert!(c.move_cursor(Point::new(20, 16)));
    assert!(!c.has_damage());
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.plane.last(), Some(&PlaneOp::Move(20, 16)));
    assert_eq!(engine.shown(), shown, "and presents no frame");
}

/// A move the plane refuses gives the pointer back to the frame, which the
/// next composite draws it into.
#[test]
fn a_refused_plane_move_hands_the_pointer_to_the_frame() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(4, 4));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    engine.plane_refuses = PlaneRefuses::Moves;
    assert!(c.move_cursor(Point::new(20, 16)));
    c.place_pointer(&mut engine);
    assert_eq!(
        engine.plane.last(),
        Some(&PlaneOp::Hide),
        "the plane is let go"
    );
    assert!(c.has_damage(), "the frame owes the pointer");
    c.present(&mut engine).expect("presents");
    assert_eq!(
        pixel(&engine.screen, output, 21, 17),
        [255, 0, 0, 255],
        "drawn in the frame"
    );
}

#[test]
fn a_trail_takes_the_pointer_off_the_plane_onto_a_layer_of_its_own() {
    let output = mode(64, 64);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
    c.set_cursor(solid_cursor(4, RED), Point::new(32, 32));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    assert!(engine.plane_up);

    c.set_pointer_trail(&[Ghost {
        at: Point::new(8, 8),
        opacity: 128,
    }]);
    let mut halo = Halo::new();
    assert!(halo.push(HaloRing {
        radius: 10,
        width: 2,
        color: Color::rgb(0, 255, 0),
    }));
    c.set_pointer_halo(&halo);
    c.present(&mut engine).expect("presents");
    assert!(!engine.plane_up, "the plane carries a pointer with no aid");
    let pointer = *engine.layers().last().expect("the pointer's layer");
    assert_eq!(
        (pointer.dst_x, pointer.dst_y),
        (8, 8),
        "the trail, the halo and the cursor together"
    );
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
            c.set_cursor(solid_cursor(4, RED), Point::new(32, 32));
            c.set_pointer_trail(&[Ghost {
                at: Point::new(8, 8),
                opacity: 128,
            }]);
            c.set_pointer_halo(&halo);
        }),
        1,
    );
}

#[test]
fn without_a_plane_the_pointer_moves_as_a_layer_without_a_bake() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(2, 2));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let first = *engine.layers().last().expect("the pointer's layer");
    assert_eq!((first.dst_x, first.dst_y), (2, 2));

    assert!(c.move_cursor(Point::new(26, 18)));
    assert!(c.has_damage(), "the pointer's layer owes its new place");
    c.present(&mut engine).expect("presents");
    let moved = *engine.layers().last().expect("the pointer's layer");
    assert_eq!((moved.dst_x, moved.dst_y), (26, 18));
    assert_eq!(moved.buffer, first.buffer, "moved, not baked");
    assert_eq!(c.frame_stats().damaged_px, 0);
}

/// A decorated window whose surface is the screen's size, windowed.
fn fullscreenable(output: DisplayMode) -> (Compositor, WindowId) {
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = c.add_window(
        Point::new(20, 20),
        opaque(output.width_px, output.height_px, RED),
    );
    let state = crate::WindowFurnitureState {
        activation: crate::WindowActivationState::Active,
        size: WindowSizeState::Restored,
        movable: true,
        resizable: true,
    };
    assert!(c.set_window_frame(id, WindowFrame::new(state)));
    assert!(c.resize_window_client(id, 40, 30));
    (c, id)
}

#[test]
fn a_fullscreen_window_is_promoted_and_what_lies_under_it_settles() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    let under = c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
    c.raise(id);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    assert!(c.on_display(id) && c.on_display(under));

    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
    let (based, stack) = engine.stacks.last().expect("a stack");
    assert!(
        !based,
        "nothing is laid beneath a window covering the output"
    );
    assert_eq!(stack.len(), 1);
    assert!(
        !c.on_display(under),
        "nothing beneath a promoted window is shown"
    );
    assert_eq!(pixel(&engine.screen, output, 0, 0), [255, 0, 0, 255]);

    assert!(c.move_window(under, Point::new(30, 30)));
    c.present(&mut engine).expect("presents");
    assert!(
        !c.has_damage(),
        "a change no one can see is held, not presented for ever"
    );

    c.set_window_size_state(id, WindowSizeState::Restored, c.screen_rect())
        .expect("restored");
    assert!(c.lower(id));
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    assert_eq!(
        pixel(&engine.screen, output, 32, 32),
        [0, 255, 0, 255],
        "what moved while hidden is shown once it can be"
    );
}

#[test]
fn promotion_waits_for_a_frame_that_genuinely_covers() {
    let output = mode(80, 60);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = c.add_window(Point::new(5, 5), opaque(40, 30, RED));
    let state = crate::WindowFurnitureState {
        activation: crate::WindowActivationState::Active,
        size: WindowSizeState::Restored,
        movable: true,
        resizable: true,
    };
    assert!(c.set_window_frame(id, WindowFrame::new(state)));
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.presentation(),
        Some(Presentation::Layered),
        "the margin the client has not painted would show whatever the scan-out last held"
    );
    assert!(c.set_surface(id, opaque(80, 60, RED)));
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
}

#[test]
fn a_translucent_fullscreen_window_is_not_promoted() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    assert!(c.set_opacity(id, 200));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    let (based, _) = engine.stacks.last().expect("a stack");
    assert!(based, "what lies behind shows through it");
}

/// An opaque red output-sized surface with a fully transparent square in it.
fn holed(output: DisplayMode) -> Surface {
    let mut surface = opaque(output.width_px, output.height_px, RED);
    for y in 10..14 {
        let (_, row) = surface.row_span_mut(y, 10, 4).expect("a row of the hole");
        row.fill(Pixel::TRANSPARENT);
    }
    surface
}

#[test]
fn a_fullscreen_window_with_a_clear_pixel_is_not_promoted() {
    let output = mode(80, 60);
    let build = |c: &mut Compositor, id: WindowId| {
        c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
            .expect("fullscreen");
        assert!(c.set_surface(id, holed(output)));
    };
    let (mut c, id) = fullscreenable(output);
    build(&mut c, id);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.presentation(),
        Some(Presentation::Layered),
        "nothing laid beneath would show the engine's black through the hole"
    );
    let (mut expected, expected_id) = fullscreenable(output);
    build(&mut expected, expected_id);
    let mut display = MockDisplay::new(output);
    expected.present(&mut display).expect("presents");
    assert_close(&engine.screen, expected.frame(), 0);
}

#[test]
fn a_fresh_fullscreen_frame_is_promoted_once_it_covers_every_pixel() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    assert!(c.set_surface(id, opaque(40, 30, RED)));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    assert!(
        !c.keeps_content(id, (80, 60)),
        "the frame at the new size is a fresh buffer"
    );
    let paint = |c: &mut Compositor, area: Rect| {
        c.present_window_content(id, 80, 60, |surface, changed| {
            surface.fill_rect(
                area.left().cast_unsigned(),
                area.top().cast_unsigned(),
                area.width,
                area.height,
                RED,
            );
            changed.add(area);
        })
        .expect("a present at the new size");
    };

    paint(&mut c, Rect::new(0, 0, 40, 60));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.presentation(),
        Some(Presentation::Layered),
        "half of the new frame is still clear"
    );
    paint(&mut c, Rect::new(40, 0, 40, 60));
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
}

/// Only a fullscreen window, the one kind promotion can take, has its content
/// read for opacity, and only while it is fullscreen.
#[test]
fn only_a_fullscreen_window_is_read_for_opacity() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    let plain = c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
    let read = |c: &Compositor, id: WindowId| {
        c.windows
            .iter()
            .find(|window| window.id() == id)
            .and_then(Window::is_opaque)
    };
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!((read(&c, id), read(&c, plain)), (None, None));

    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    c.present(&mut engine).expect("presents");
    assert_eq!(read(&c, plain), None);
    assert!(read(&c, id).is_some(), "the fullscreen window is read");

    c.set_window_size_state(id, WindowSizeState::Restored, c.screen_rect())
        .expect("restored");
    c.present(&mut engine).expect("presents");
    assert_eq!(read(&c, id), None, "and forgotten once it is not");
}

#[test]
fn a_queued_present_holds_the_next_frame_until_the_display_is_ready() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    engine.ready = false;
    assert!(c.move_window(top, Point::new(20, 12)));
    c.present(&mut engine).expect("held");
    assert_eq!(
        engine.stacks.len(),
        1,
        "nothing is drawn into buffers the display still reads"
    );
    assert!(c.has_damage(), "the frame waits for the display");
    engine.ready = true;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.stacks.len(), 2);
    assert!(!c.has_damage());
}

#[test]
fn a_reveal_presents_the_frame_and_gives_the_layers_back() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(!engine.buffers.is_empty());

    assert!(c.set_reveal(128));
    c.present(&mut engine).expect("presents");
    assert!(
        engine.buffers.is_empty(),
        "every layer's buffers were given back"
    );
    assert_eq!(engine.frames, 1, "the dimmed frame was presented whole");
    assert_eq!(
        pixel(&engine.frame, output, 20, 14),
        [100, 100, 0, 255],
        "the window that had a layer is in the frame again, dimmed"
    );
    assert!(c.set_reveal(u8::MAX));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        engine.stacks.len(),
        2,
        "back on layers once the fade is over"
    );
}

/// An engine that lays a layer at an opacity of its own.
fn fading_engine(output: DisplayMode) -> MockEngine {
    let mut engine = MockEngine::new(output);
    engine.caps.per_layer_opacity = true;
    engine
}

/// Windows on layers, a frosted one among them, and the pointer over them.
fn fading_scene(c: &mut Compositor) {
    three(c);
    let glass = c.add_window(Point::new(4, 6), glass(16, 8));
    assert!(c.set_backdrop_blur(glass, 2));
    c.set_cursor(
        solid_cursor(4, Color::rgb(250, 250, 250)),
        Point::new(18, 10),
    );
}

/// What `engine`'s display frames have been drawn so far.
fn draws(engine: &MockEngine) -> usize {
    engine.drawn.as_ref().map_or(0, |frames| frames.draws)
}

#[test]
fn a_fade_through_the_engine_s_opacity_moves_no_pixel_after_its_first_step() {
    // Its first step flattens the layers into the base; every step after it
    // hands the engine an opacity and nothing else.
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    fading_scene(&mut c);
    let mut engine = fading_engine(output);
    c.present(&mut engine).expect("presents");
    assert!(engine.layers().len() >= 2, "the windows ride layers");
    for (step, reveal) in [200u8, 150, 90, 30, 0, 60, 180].into_iter().enumerate() {
        let (buffers, drawn) = (snapshot(&engine), draws(&engine));
        assert!(c.set_reveal(reveal));
        c.present(&mut engine).expect("presents");
        let (based, stack) = engine.stacks.last().expect("a stack");
        assert!(*based && stack.len() == 1, "step {step}: the base alone");
        assert_eq!(stack[0].opacity, reveal, "step {step}");
        if step > 0 {
            assert_eq!(draws(&engine), drawn, "step {step}: no frame drawn");
            assert!(written(&engine, &buffers).is_empty(), "step {step}");
            assert_eq!(c.frame_stats().encoded_px, 0, "step {step}");
        }
        let faded = software(output, BLUE, |c| {
            fading_scene(c);
            c.set_reveal(reveal);
        });
        assert_close(&engine.screen, &faded, 1);
    }
    assert!(c.set_reveal(u8::MAX));
    c.present(&mut engine).expect("presents");
    assert!(engine.layers().len() >= 2, "back on layers once it is over");
    assert_close(&engine.screen, &software(output, BLUE, fading_scene), 1);
}

#[test]
fn a_promoted_window_fades_through_its_own_layer() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    let mut engine = fading_engine(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
    let before = snapshot(&engine);
    assert!(c.set_reveal(100));
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
    let (based, stack) = engine.stacks.last().expect("a stack");
    assert!(!based && stack.len() == 1, "its own layer alone");
    assert_eq!(stack[0].opacity, 100);
    assert!(
        written(&engine, &before).is_empty(),
        "its pixels are untouched"
    );
    assert_eq!(
        pixel(&engine.screen, output, 5, 5),
        [faded(255, 100), 0, 0, 255]
    );
}

#[test]
fn a_pointer_moving_through_a_fade_redraws_only_its_own_pixels() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    fading_scene(&mut c);
    let mut engine = fading_engine(output);
    c.present(&mut engine).expect("presents");
    assert!(c.set_reveal(120));
    c.present(&mut engine).expect("presents");
    c.move_cursor(Point::new(6, 3));
    c.present(&mut engine).expect("presents");
    let stats = c.frame_stats();
    assert!(
        stats.encoded_px > 0 && stats.encoded_px <= 2 * 4 * 4,
        "where it was and where it is, at full strength: {stats:?}"
    );
    assert_eq!(engine.stacks.last().expect("a stack").1[0].opacity, 120);
    let faded = software(output, BLUE, |c| {
        fading_scene(c);
        c.move_cursor(Point::new(6, 3));
        c.set_reveal(120);
    });
    assert_close(&engine.screen, &faded, 1);
}

#[test]
fn a_fade_the_engine_stops_taking_is_finished_in_software_at_its_strength() {
    // The frames the engine faded hold the screen at full strength, so the
    // composite taking over encodes all of it again at the reveal.
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    fading_scene(&mut c);
    let mut engine = fading_engine(output);
    c.present(&mut engine).expect("presents");
    assert!(c.set_reveal(150));
    c.present(&mut engine).expect("presents");
    engine.refusal = Some(DriverError::Unsupported);
    c.move_cursor(Point::new(6, 3));
    c.present(&mut engine).expect("presents");
    let faded = software(output, BLUE, |c| {
        fading_scene(c);
        c.move_cursor(Point::new(6, 3));
        c.set_reveal(150);
    });
    assert_close(&engine.screen, &faded, 1);
}

#[test]
fn a_smaller_desktop_is_scaled_by_an_engine_that_scales() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(2, 2), opaque(4, 4, RED));
    assert!(c.set_desktop_size(16, 12));
    let mut engine = MockEngine::new(output);
    engine.caps.hw_scaling = true;
    c.present(&mut engine).expect("presents");
    let (based, stack) = engine.stacks.last().expect("a stack");
    assert!(!based);
    assert_eq!(stack.len(), 1, "the desktop alone");
    let desktop = stack[0];
    assert_eq!((desktop.src.width_px, desktop.src.height_px), (16, 12));
    assert_eq!((desktop.dst_width_px, desktop.dst_height_px), (32, 24));
    assert_eq!(pixel(&engine.screen, output, 5, 5), [255, 0, 0, 255]);
    assert_eq!(pixel(&engine.screen, output, 20, 20), [0, 0, 255, 255]);
}

#[test]
fn a_desktop_the_engine_scales_fades_through_its_own_layer() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(2, 2), opaque(4, 4, RED));
    assert!(c.set_desktop_size(16, 12));
    let mut engine = fading_engine(output);
    engine.caps.hw_scaling = true;
    c.present(&mut engine).expect("presents");
    let before = snapshot(&engine);
    assert!(c.set_reveal(90));
    c.present(&mut engine).expect("presents");
    let (based, stack) = engine.stacks.last().expect("a stack");
    assert!(!based && stack.len() == 1, "the desktop alone");
    assert_eq!(stack[0].opacity, 90);
    assert!(written(&engine, &before).is_empty(), "no pixel moved");
    assert_eq!(
        pixel(&engine.screen, output, 5, 5),
        [faded(255, 90), 0, 0, 255]
    );
}

/// A scaled desktop the display cannot give a layer is composited, and its
/// layer is not asked for again at the same size.
#[test]
fn a_desktop_refused_its_layer_is_not_asked_for_it_again() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = c.add_window(Point::new(2, 2), opaque(4, 4, RED));
    assert!(c.set_desktop_size(16, 12));
    let mut engine = MockEngine::new(output);
    engine.caps.hw_scaling = true;
    engine.room = 0;
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Composited));
    let asked = engine.allocs_asked;
    assert!(c.move_window(id, Point::new(4, 4)));
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.allocs_asked, asked, "not asked again");
    assert_eq!(c.presentation(), Some(Presentation::Composited));
}

#[test]
fn a_smaller_desktop_is_resampled_by_the_compositor_where_the_engine_cannot_scale() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(2, 2), opaque(4, 4, RED));
    assert!(c.set_desktop_size(16, 12));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(
        engine.stacks.is_empty(),
        "no layer was placed in desktop pixels"
    );
    assert_eq!(c.presentation(), Some(Presentation::Composited));
}

#[test]
fn a_display_replaced_under_the_layers_is_given_them_anew() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut first = MockEngine::new(output);
    c.present(&mut first).expect("presents");
    let mut second = MockEngine::new(output);
    second.next = 100;
    c.present(&mut second).expect("presents");
    assert!(
        !second.buffers.is_empty(),
        "layers allocated from the display now serving"
    );
    assert_close(
        &second.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );
}

#[test]
fn a_stack_the_engine_can_never_take_keeps_the_output_composited() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [.., top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.refusal = Some(DriverError::Unsupported);
    c.present(&mut engine).expect("the frame goes instead");
    assert_eq!(engine.frames, 1);
    assert!(engine.buffers.is_empty());
    engine.refusal = None;
    assert!(c.move_window(top, Point::new(20, 12)));
    c.present(&mut engine).expect("presents");
    assert!(engine.stacks.is_empty(), "the engine is not asked again");
    assert_eq!(engine.frames, 2);
}

#[test]
fn a_refused_layered_frame_is_owed_to_the_next_present() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.refusal = Some(DriverError::DeviceFault);
    assert_eq!(c.present(&mut engine), Err(DriverError::DeviceFault));
    assert!(c.has_damage(), "the frame is still owed");
    engine.refusal = None;
    c.present(&mut engine).expect("presents");
    assert!(!c.has_damage());
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );
}

#[test]
fn a_layered_frame_settles_a_frame_the_display_refused() {
    let output = mode(8, 8);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(1, 1), opaque(2, 2, RED));
    let mut refusing = MockDisplay::new(output);
    refusing.fail = true;
    assert_eq!(c.present(&mut refusing), Err(DriverError::DeviceFault));
    assert!(c.has_damage(), "the refused frame is owed");
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("the layered present");
    assert!(!c.has_damage(), "nothing is owed once the engine holds it");
}

#[test]
fn a_casters_layer_spans_its_footprint_and_carries_its_shadow() {
    let output = mode(200, 160);
    let build = |c: &mut Compositor| {
        let id = c.add_window(Point::new(60, 40), opaque(60, 40, RED));
        assert!(c.set_casts_shadow(id, true));
    };
    let grey = Color::rgb(0x80, 0x80, 0x80);
    let mut c = new_compositor(output, grey).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let layer = *engine.layers().last().expect("the caster's layer");
    let footprint = crate::shadow_footprint(Rect::new(60, 40, 60, 40), c.scale(), c.theme());
    assert_eq!(
        (
            layer.dst_x,
            layer.dst_y,
            layer.src.width_px,
            layer.src.height_px
        ),
        (
            footprint.left(),
            footprint.top(),
            footprint.width,
            footprint.height
        ),
        "the layer is the footprint, not the window's rectangle"
    );
    let shadow = layer_pixel(
        &engine,
        &layer,
        (90 - footprint.left()).cast_unsigned(),
        (82 - footprint.top()).cast_unsigned(),
    );
    assert_eq!(
        &shadow[..3],
        &[0, 0, 0],
        "the shadow is premultiplied black"
    );
    assert!(
        shadow[3] > 0 && shadow[3] < 255,
        "and translucent: {shadow:?}"
    );
    assert_close(&engine.screen, &software(output, grey, build), 1);
}

#[test]
fn the_desktop_layer_lies_in_the_base_beneath_the_window_layers() {
    let output = mode(16, 16);
    let build = |c: &mut Compositor| {
        c.set_desktop(opaque(16, 16, GREEN));
        c.add_window(Point::new(2, 2), opaque(4, 4, RED));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.layers().len(), 1, "the window over the base");
    assert_eq!(
        pixel(&engine.frame, output, 8, 8),
        [0, 255, 0, 255],
        "the desktop is the base's"
    );
    assert_close(&engine.screen, &software(output, BLUE, build), 0);
}

/// A pointer no layer can carry — an engine with no room for one beside a
/// window, and no cursor plane — is the frame's, so no window goes on a
/// layer above it, and a frame with nothing new presents nothing.
#[test]
fn a_pointer_no_layer_can_carry_is_drawn_in_the_frame() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(28, 2));
    let mut engine = MockEngine::new(output);
    engine.caps.max_layers = 2;
    c.present(&mut engine).expect("presents");
    assert!(
        engine.stacks.is_empty(),
        "no window may lie above the pointer"
    );
    assert_eq!(pixel(&engine.screen, output, 29, 3), [255, 0, 0, 255]);
    assert!(!c.has_damage(), "the frame settles");

    assert!(c.move_cursor(Point::new(26, 18)));
    c.present(&mut engine).expect("presents");
    assert!(engine.stacks.is_empty());
    assert_eq!(pixel(&engine.screen, output, 27, 19), [255, 0, 0, 255]);
    assert_eq!(pixel(&engine.screen, output, 29, 3), [0, 0, 255, 255]);
}

/// A window the display cannot give buffers joins the base with everything
/// beneath it, the windows above keep their layers, and it is not asked for
/// them again until its size changes.
#[test]
fn a_window_refused_its_buffers_joins_the_base_and_is_not_asked_again() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [bottom, _, top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.room = 4;
    c.present(&mut engine).expect("presents");
    let layers = engine.layers();
    assert_eq!(
        layers.len(),
        2,
        "the two windows the display had buffers for"
    );
    assert_eq!((layers[0].dst_x, layers[0].dst_y), (6, 4));
    assert_eq!((layers[1].dst_x, layers[1].dst_y), (14, 8));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );

    let asked = engine.next;
    assert!(c.move_window(top, Point::new(18, 10)));
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.next, asked, "no buffer asked for again");
    assert_eq!(engine.layers().len(), 2);

    assert!(c.resize_window_client(bottom, 4, 4));
    engine.room = usize::MAX;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.layers().len(), 3, "asked again at its new size");
}

/// A stack the engine cannot composite at once gives its lowest window to
/// the base within the frame, the windows above keep their buffers, and that
/// window is not layered again until its size changes.
#[test]
fn a_stack_too_deep_to_composite_at_once_gives_its_lowest_window_to_the_base() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let [bottom, middle, top] = three(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.layers().len(), 3);

    // A frame carrying damage beneath the layers is refused, and none of it
    // is lost on the way to the stack that is taken.
    assert!(c.set_surface(middle, opaque(10, 10, RED)));
    engine.deepest = 2;
    let asked = engine.next;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.frames, 0, "layered within the frame");
    assert_eq!(engine.next, asked, "the windows above keep their buffers");
    let layers = engine.layers();
    assert_eq!(layers.len(), 2, "the two windows above");
    assert_eq!((layers[0].dst_x, layers[0].dst_y), (6, 4));
    assert_eq!((layers[1].dst_x, layers[1].dst_y), (14, 8));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            let [_, middle, _] = three(c);
            assert!(c.set_surface(middle, opaque(10, 10, RED)));
        }),
        0,
    );

    let asked = engine.next;
    assert!(c.move_window(top, Point::new(18, 10)));
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.next, asked, "no buffer asked for again");
    assert_eq!(engine.layers().len(), 2);

    assert!(c.resize_window_client(bottom, 4, 4));
    engine.deepest = usize::MAX;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.layers().len(), 3, "layered again at its new size");
}

/// A layer with nothing baked to show refuses the present it would be left
/// out of, so its window is never missing from a frame: a bake that could not
/// have its row is owed to the next frame instead.
#[test]
fn a_layer_with_nothing_baked_refuses_the_present_it_would_be_missing_from() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(2, 2), opaque(3, 3, RED));
    let wide = c.add_window(Point::new(8, 6), opaque(20, 12, GREEN));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(!c.has_damage());
    let stack = c.layers.as_mut().expect("layered");
    stack.scratch = Vec::new();
    let layer = stack
        .layers
        .iter_mut()
        .find(|layer| layer.window() == Some(wide))
        .expect("the wide window's layer");
    (layer.staged, layer.shown) = (None, None);
    layer.dirty_all();
    let plan = c.plan_layers(&engine.caps, false).expect("a plan");
    let row = core::mem::size_of::<Pixel>();
    assert_eq!(
        tairix_fuzzseed::meter::refusing_above(row, || c.present_plan(&mut engine, &plan)),
        Err(DriverError::OutOfMemory)
    );
    assert_eq!(engine.stacks.len(), 1, "nothing presented without it");
}

/// A stack the engine refuses once a refused window is in the base blames
/// a window that is on a layer, so the frame is planned again with fewer.
#[test]
fn a_stack_too_deep_blames_a_window_on_a_layer() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    let mut engine = MockEngine::new(output);
    engine.room = 4;
    engine.deepest = 1;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.frames, 0, "layered");
    assert_eq!(engine.stacks_asked, 2, "never the refused stack twice");
    let layers = engine.layers();
    assert_eq!(layers.len(), 1, "the top window alone");
    assert_eq!((layers[0].dst_x, layers[0].dst_y), (14, 8));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            three(c);
        }),
        0,
    );
}

/// A fade's end owes the whole screen a re-encode, which a promoted window
/// covers: it is held for the base, so the desktop settles.
#[test]
fn a_promoted_window_settles_once_a_fade_is_over() {
    let output = mode(80, 60);
    let (mut c, id) = fullscreenable(output);
    c.set_window_size_state(id, WindowSizeState::Fullscreen, c.screen_rect())
        .expect("fullscreen");
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));

    assert!(c.set_reveal(128));
    c.present(&mut engine).expect("presents");
    assert!(c.set_reveal(u8::MAX));
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Promoted(id)));
    assert!(
        !c.has_damage(),
        "nothing is owed while the window covers it"
    );
}

/// A pointer growing within the buffers of its layer — a cursor enlarged a
/// step at a time — keeps them, cut to its size.
#[test]
fn a_pointer_growing_within_its_layer_keeps_its_buffers() {
    let output = mode(64, 64);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
    c.set_cursor(solid_cursor(4, RED), Point::new(32, 32));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let small = *engine.layers().last().expect("the pointer's layer");
    assert_eq!((small.dst_width_px, small.dst_height_px), (4, 4));

    let asked = engine.next;
    c.set_cursor(solid_cursor(12, RED), Point::new(32, 32));
    c.present(&mut engine).expect("presents");
    let grown = *engine.layers().last().expect("the pointer's layer");
    assert_eq!(engine.next, asked, "no buffer asked for");
    assert_eq!((grown.dst_width_px, grown.dst_height_px), (12, 12));
    assert_eq!((grown.src.width_px, grown.src.height_px), (12, 12));
    assert_close(
        &engine.screen,
        &software(output, BLUE, |c| {
            c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
            c.set_cursor(solid_cursor(12, RED), Point::new(32, 32));
        }),
        1,
    );
}

/// A cursor plane that refuses is not asked again for the same pointer:
/// the pointer rides a layer, and the plane is tried once more only when the
/// pointer changes shape.
#[test]
fn a_refusing_cursor_plane_is_asked_again_only_for_a_new_shape() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(28, 2));
    let mut engine = MockEngine::new(output).with_cursor();
    engine.plane_refuses = PlaneRefuses::Images;
    c.present(&mut engine).expect("presents");
    assert_eq!(engine.plane_asks, 1);
    assert_eq!(pixel(&engine.screen, output, 29, 3), [255, 0, 0, 255]);

    assert!(c.move_cursor(Point::new(26, 18)));
    c.place_pointer(&mut engine);
    c.present(&mut engine).expect("presents");
    assert_eq!(
        engine.plane_asks, 1,
        "the same shape is not handed over again"
    );

    engine.plane_refuses = PlaneRefuses::Nothing;
    c.set_cursor(solid_cursor(6, RED), Point::new(26, 18));
    c.place_pointer(&mut engine);
    assert_eq!(engine.plane_asks, 2);
    assert!(engine.plane_up);
}

/// The cursor plane is not faded with the screen, so through a fade the
/// pointer is the frame's.
#[test]
fn a_fade_takes_the_pointer_off_the_cursor_plane() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    three(&mut c);
    c.set_cursor(solid_cursor(4, RED), Point::new(28, 2));
    let mut engine = MockEngine::new(output).with_cursor();
    c.present(&mut engine).expect("presents");
    assert!(engine.plane_up);

    assert!(c.set_reveal(128));
    c.present(&mut engine).expect("presents");
    assert!(!engine.plane_up, "the plane would show it undimmed");
    assert_eq!(engine.plane.last(), Some(&PlaneOp::Hide));
    assert_eq!(
        pixel(&engine.screen, output, 29, 3),
        [128, 0, 0, 255],
        "dimmed with the frame"
    );
}

/// A window resized while its layer still shows it is repainted whole once
/// it joins the base: where its layer lay, and where it now reaches.
#[test]
fn a_window_resized_on_its_layer_is_repainted_whole_in_the_base() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = c.add_window(Point::new(2, 2), opaque(6, 6, RED));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(!engine.stacks.is_empty());

    assert!(c.resize_window_client(id, 16, 12));
    assert!(c.set_reveal(128));
    c.present(&mut engine).expect("presents");
    assert!(c.set_reveal(u8::MAX));
    let expected = software(output, BLUE, |c| {
        let id = c.add_window(Point::new(2, 2), opaque(6, 6, RED));
        assert!(c.resize_window_client(id, 16, 12));
    });
    let mut display = MockDisplay::new(output);
    c.present(&mut display).expect("presents");
    assert_close(c.frame(), &expected, 0);
}

/// A pointer bake that cannot have its scratch row leaves the buffer it was
/// baking stale and unstaged, rather than showing what it did not draw.
#[test]
fn a_pointer_bake_short_of_memory_stages_nothing() {
    let output = mode(64, 64);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    c.add_window(Point::new(4, 4), opaque(10, 10, GREEN));
    c.set_cursor(solid_cursor(4, RED), Point::new(32, 32));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    c.set_cursor(solid_cursor(32, RED), Point::new(32, 32));
    let staged = |c: &Compositor| {
        c.layers
            .as_ref()
            .and_then(|stack| {
                stack
                    .layers
                    .iter()
                    .find(|layer| layer.source == Source::Pointer)
            })
            .map(|layer| (layer.staged, layer.target()))
    };
    let row = core::mem::size_of::<Pixel>() * 32;
    tairix_fuzzseed::meter::refusing_above(row - 1, || c.bake_pointer(&mut engine));
    let (unstaged, owed) = staged(&c).expect("the pointer's layer");
    assert_eq!(unstaged, None, "nothing it did not draw is staged");
    assert!(owed.is_some(), "the bake is still owed");

    c.bake_pointer(&mut engine);
    let (finished, settled) = staged(&c).expect("the pointer's layer");
    assert!(finished.is_some(), "staged once the row could be had");
    assert_eq!(settled, None);
}

/// An opaque decorated window over grey: a free layer carrying its frame and
/// its shadow.
fn framed(c: &mut Compositor) -> WindowId {
    let id = c.add_window(Point::new(60, 40), opaque(60, 40, RED));
    assert!(c.set_window_frame(id, WindowFrame::new(decorated())));
    id
}

/// Repaint a corner of `id`'s client in `shade`, as a clock's tick repaints
/// its digits.
fn tick(c: &mut Compositor, id: WindowId, shade: u8) {
    let mut area = Region::new();
    area.add(Rect::new(2, 2, 3, 3));
    assert!(c.repaint_window(id, (60, 40), &area, |surface, rects| {
        for rect in rects {
            surface.fill_rect(
                rect.left().cast_unsigned(),
                rect.top().cast_unsigned(),
                rect.width,
                rect.height,
                Color::rgb(shade, shade, shade),
            );
        }
    }));
}

/// `framed`'s window on a free layer of `engine`, presented once, with each
/// later present held to the software composite of `look` and then of two
/// ticks: a tick bakes the buffer the engine is not showing, so a look that
/// reached only one of a layer's two buffers flips with every tick.
fn holds_through_ticks(look: impl Fn(&mut Compositor)) {
    let output = mode(200, 160);
    let grey = Color::rgb(0x80, 0x80, 0x80);
    let mut c = new_compositor(output, grey).expect("compositor");
    let id = framed(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(
        c.layers.as_ref().is_some_and(|stack| stack
            .layers
            .iter()
            .any(|layer| layer.source == Source::Window(id, super::Role::Free))),
        "the window has a free layer"
    );

    look(&mut c);
    c.present(&mut engine).expect("presents");
    let restyled = software(output, grey, |c| {
        framed(c);
        look(c);
    });
    assert_close(&engine.screen, &restyled, 1);
    for shade in [9, 10] {
        tick(&mut c, id, shade);
        c.present(&mut engine).expect("presents");
        let expected = software(output, grey, |c| {
            let id = framed(c);
            look(c);
            tick(c, id, shade);
        });
        assert_close(&engine.screen, &expected, 1);
    }
}

/// An opaque framed window on a free layer under a frosted framed one, focus
/// passing between them while the first keeps presenting and is retitled:
/// every present is held to the software composite, and the two after the
/// last change catch a buffer of the free layer left behind.
#[test]
fn a_free_layer_under_a_frost_keeps_its_title_bar_as_focus_moves() {
    type Step = fn(&mut Compositor, WindowId, WindowId);
    let output = mode(200, 160);
    let grey = Color::rgb(0x80, 0x80, 0x80);
    let scene = |c: &mut Compositor| {
        let map = framed(c);
        let board = c.add_window(Point::new(90, 10), clear(80, 50));
        assert!(c.set_window_frame(board, WindowFrame::new(decorated())));
        assert!(c.set_backdrop_blur(board, 2));
        (map, board)
    };
    let steps: [Step; 6] = [
        |c, _, board| {
            c.follow_focus(Some(board));
            assert!(c.raise(board));
            c.step_elevations(0);
            c.step_elevations(SETTLED_NS);
        },
        |c, map, _| tick(c, map, 9),
        |c, map, _| {
            c.follow_focus(Some(map));
            assert!(c.raise(map));
            c.step_elevations(0);
            c.step_elevations(SETTLED_NS);
        },
        |c, map, _| assert!(c.set_window_title(map, "Network map")),
        |c, map, _| tick(c, map, 10),
        |c, map, _| tick(c, map, 11),
    ];
    let mut c = new_compositor(output, grey).expect("compositor");
    let (map, board) = scene(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert!(
        c.layers.as_ref().is_some_and(|stack| stack
            .layers
            .iter()
            .any(|layer| layer.source == Source::Window(map, super::Role::Free))),
        "the opaque window has a free layer"
    );
    for done in 1..=steps.len() {
        steps[done - 1](&mut c, map, board);
        c.present(&mut engine).expect("presents");
        let expected = software(output, grey, |c| {
            let (map, board) = scene(c);
            for step in &steps[..done] {
                step(c, map, board);
            }
        });
        // A focused window's deeper shadow is rounded on its layer and again
        // by the engine, where software rounds it once.
        assert_close(&engine.screen, &expected, 2);
    }
}

#[test]
fn a_shadow_coming_to_rest_and_a_frame_dimming_reach_both_buffers_of_a_free_layer() {
    holds_through_ticks(|c| {
        c.follow_focus(None);
        c.step_elevations(0);
        c.step_elevations(SETTLED_NS);
    });
}

#[test]
fn a_new_theme_is_baked_into_both_buffers_of_a_free_layer() {
    holds_through_ticks(|c| {
        assert!(c.set_theme(tairix_theme::Theme::light()));
    });
}

#[test]
fn a_new_scale_leaves_both_buffers_of_a_free_layer_owing_a_whole_bake() {
    let output = mode(200, 160);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let id = c.add_window(Point::new(20, 20), opaque(40, 30, RED));
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");

    assert!(c.set_scale(tairix_geometry::Scale::from_percent(150).expect("scale")));
    let layer = c
        .layers
        .as_ref()
        .and_then(|stack| {
            stack
                .layers
                .iter()
                .find(|layer| layer.source == Source::Window(id, super::Role::Free))
        })
        .expect("the window keeps its free layer");
    let whole = Rect::new(0, 0, layer.at.width, layer.at.height);
    for held in &layer.held {
        assert_eq!(held.stale.rects(), [whole]);
    }
}
