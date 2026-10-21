//! The layered present against an engine that composites what it is handed,
//! so each scene is held to the software composite of the same scene.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageRect, DisplayMode, DisplayPower,
};
use tairix_abi::DriverError;
use tairix_display::{Base, CursorPlane, LayerPresenter, Presenter};

use crate::color::{Color, Pixel};
use crate::geometry::{Point, Rect, Region};
use crate::pointer::{Ghost, Halo, HaloRing};
use crate::surface::Surface;
use crate::tests::{
    decorated, mode, new_compositor, opaque, solid_cursor, MockDisplay, SETTLED_NS,
};
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
}

const CAPS: AccelCaps = AccelCaps {
    max_layers: 8,
    max_width_px: 1024,
    max_height_px: 1024,
    per_layer_opacity: false,
    hw_scaling: false,
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
        }
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
                let src = &bytes[(sy * bw + sx) * 4..(sy * bw + sx) * 4 + 4];
                let at = y as usize * stride + x as usize * 4;
                let keep = 255 - u32::from(src[3]);
                for (under, laid) in self.screen[at..at + 4].iter_mut().zip(src) {
                    let blended = u32::from(*laid) + (u32::from(*under) * keep + 127) / 255;
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
        base: Option<Base<'_>>,
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
        match base {
            Some(base) => {
                self.take(base.damage, base.frame);
                self.screen = self.frame.clone();
            }
            None => {
                for pixel in self.screen.as_chunks_mut::<4>().0 {
                    *pixel = [0, 0, 0, 255];
                }
            }
        }
        for layer in layers {
            self.lay(layer);
        }
        let mut stack = Vec::new();
        if base.is_some() {
            stack.push(AccelLayer {
                buffer: BufferId::new(u32::MAX).expect("a marker"),
                src: DamageRect::full(&self.mode),
                dst_x: 0,
                dst_y: 0,
                dst_width_px: self.mode.width_px,
                dst_height_px: self.mode.height_px,
                opacity: u8::MAX,
            });
        }
        stack.extend_from_slice(layers);
        self.stacks.push((base.is_some(), stack));
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
        pixel(c.frame(), output, 7, 5),
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

/// A fully transparent surface, which frosted shows nothing but its frost.
fn clear(w: u32, h: u32) -> Surface {
    Surface::filled(w, h, Pixel::TRANSPARENT).expect("surface allocates")
}

#[test]
fn a_frosted_window_is_flattened_with_its_frost() {
    let output = mode(24, 12);
    let build = |c: &mut Compositor| {
        c.add_window(Point::ORIGIN, opaque(12, 12, RED));
        let glass = c.add_window(Point::new(4, 2), clear(16, 8));
        assert!(c.set_backdrop_blur(glass, 2));
    };
    let mut c = new_compositor(output, BLUE).expect("compositor");
    build(&mut c);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    assert_eq!(c.presentation(), Some(Presentation::Layered));
    assert_close(&engine.screen, &software(output, BLUE, build), 1);
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
fn partially_transparent_content_is_flattened_until_it_is_painted_opaque() {
    let output = mode(32, 24);
    let mut c = new_compositor(output, BLUE).expect("compositor");
    let veil = Surface::filled(8, 8, Color::rgba(255, 0, 0, 128).premultiply()).expect("surface");
    let id = c.add_window(Point::new(4, 4), veil);
    let mut engine = MockEngine::new(output);
    c.present(&mut engine).expect("presents");
    let flattened = *engine.layers().last().expect("its layer");
    assert_eq!(
        layer_pixel(&engine, &flattened, 0, 0)[3],
        255,
        "composited opaque"
    );

    let mut area = Region::new();
    area.add(Rect::new(0, 0, 8, 8));
    assert!(c.repaint_window(id, (8, 8), &area, |surface, _| {
        surface.fill_rect(0, 0, 8, 8, RED);
    }));
    c.present(&mut engine).expect("presents");
    let free = *engine.layers().last().expect("its layer");
    assert_ne!(free.buffer, flattened.buffer, "a free layer of its own");
    assert_eq!(c.frame_stats().damaged_px, 64, "the base under it, once");
    assert!(c.move_window(id, Point::new(10, 10)));
    c.present(&mut engine).expect("presents");
    assert_eq!(
        c.frame_stats().damaged_px,
        0,
        "a free layer moves without a composite"
    );
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
        pixel(c.frame(), output, 20, 14),
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
        pixel(c.frame(), output, 8, 8),
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
