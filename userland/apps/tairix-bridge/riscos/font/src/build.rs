//! First-party Acorn fonts made from geometry: a metrics file and a version
//! 8 outlines file for each font, in the layouts the PRM gives, and bitmap
//! files of glyphs given pixel by pixel, new style and old. The fonts the
//! tests read and the corpus has genuine RISC OS draw are made here, so no
//! font of RISC OS's is ever needed to test against.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::outlines::CHUNK;
pub use crate::outlines::{Element, Point};

/// The scaffold table's size: its size word, the skeleton threshold and a
/// byte of padding, with no scaffold lines.
const TABLE_BYTES: u16 = 4;

/// One character of a font made here.
#[derive(Clone, Debug, Default)]
pub struct Character {
    /// Where it moves the pen, in thousandths of an em.
    pub advance: i16,
    /// Its outline, filled, in design units.
    pub fill: Vec<Element>,
    /// Paths stroked thin.
    pub strokes: Vec<Element>,
    /// Characters drawn with it: each code and its offset.
    pub inclusions: Vec<(u8, Point)>,
}

/// A font made here: its name, its design size, its characters by code
/// and its kern pairs.
#[derive(Clone, Debug)]
pub struct Font {
    /// The name its metrics file gives it.
    pub name: Vec<u8>,
    /// Design units an em.
    pub design_size: u16,
    /// Its characters, by the glyph code each is at.
    pub characters: BTreeMap<u8, Character>,
    /// Its kern pairs: the left glyph, the right, and the kern in
    /// thousandths of an em.
    pub kerns: Vec<(u8, u8, i16)>,
}

/// The rectangle from `(x0, y0)` to `(x1, y1)`, in design units.
#[must_use]
pub fn rectangle(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<Element> {
    alloc::vec![
        Element::Move(Point::new(x0, y0)),
        Element::Line(Point::new(x1, y0)),
        Element::Line(Point::new(x1, y1)),
        Element::Line(Point::new(x0, y1)),
    ]
}

/// A character advancing `advance` with outline `fill`.
#[must_use]
pub fn filled(advance: i16, fill: Vec<Element>) -> Character {
    Character {
        advance,
        fill,
        ..Character::default()
    }
}

/// The font DrawFile's text was probed in: a box for every letter and digit
/// of an advance it shares with its kind, so a line's breaks and its every
/// character's place are plain from its paint.
#[must_use]
pub fn lay() -> Font {
    let mut characters = BTreeMap::new();
    characters.insert(b' ', filled(250, Vec::new()));
    characters.insert(b'-', filled(300, rectangle(20, 200, 280, 300)));
    characters.insert(b'.', filled(200, rectangle(50, 0, 150, 100)));
    characters.insert(b'\\', filled(300, rectangle(0, 0, 300, 700)));
    for digit in b'0'..=b'9' {
        characters.insert(digit, filled(500, rectangle(50, 0, 450, 600)));
    }
    for upper in b'A'..=b'Z' {
        characters.insert(upper, filled(700, rectangle(50, 0, 650, 700)));
    }
    for lower in b'a'..=b'z' {
        characters.insert(lower, filled(500, rectangle(50, 0, 450, 500)));
    }
    characters.insert(b'i', filled(250, rectangle(75, 0, 175, 500)));
    characters.insert(b'm', filled(800, rectangle(50, 0, 750, 500)));
    characters.insert(b'w', filled(750, rectangle(50, 0, 700, 500)));
    Font {
        name: b"Lay".to_vec(),
        design_size: 1000,
        characters,
        kerns: Vec::new(),
    }
}

/// The font the painting probes drew: at 16 points and 90 dots an inch a
/// pixel is 50 thousandths of an em, so `A` is four pixels by eight.
#[must_use]
pub fn pix() -> Font {
    let mut characters = BTreeMap::new();
    characters.insert(b' ', filled(300, Vec::new()));
    characters.insert(b'A', filled(300, rectangle(0, 0, 200, 400)));
    let corner = alloc::vec![
        Element::Move(Point::new(0, 0)),
        Element::Line(Point::new(300, 0)),
        Element::Line(Point::new(0, 300)),
    ];
    characters.insert(b'F', filled(400, corner));
    characters.insert(b'G', filled(300, rectangle(0, -100, 200, 0)));
    Font {
        name: b"Pix".to_vec(),
        design_size: 1000,
        characters,
        kerns: Vec::new(),
    }
}

/// The font the buffer probes wrote: `A` a curve, `B` stroke paths alone,
/// `C` the four pixels by eight `Pix`'s `A` is.
#[must_use]
pub fn cv() -> Font {
    let mut characters = BTreeMap::new();
    characters.insert(b' ', filled(300, Vec::new()));
    let curve = alloc::vec![
        Element::Move(Point::new(0, 0)),
        Element::Curve(Point::new(300, 0), Point::new(300, 300), Point::new(0, 300)),
    ];
    characters.insert(b'A', filled(400, curve));
    characters.insert(
        b'B',
        Character {
            advance: 400,
            strokes: alloc::vec![
                Element::Move(Point::new(0, 0)),
                Element::Line(Point::new(300, 300)),
                Element::Line(Point::new(300, 0)),
            ],
            ..Character::default()
        },
    );
    characters.insert(b'C', filled(300, rectangle(0, 0, 200, 400)));
    Font {
        name: b"Cv".to_vec(),
        design_size: 1000,
        characters,
        kerns: Vec::new(),
    }
}

/// Where the reference corpus's font library is staged, on HostFS and the
/// replay's disc alike.
pub const CORPUS_LIBRARY: &str = "FntLib";

/// The font library the Font Manager's reference corpus is painted from,
/// genuine RISC OS's and this one's alike: each file's path within it,
/// typed as HostFS names it, and its bytes.
#[must_use]
pub fn corpus_library() -> Vec<(alloc::string::String, Vec<u8>)> {
    let mut files = Vec::new();
    for font in [lay(), pix(), cv()] {
        let name = alloc::string::String::from_utf8_lossy(&font.name).into_owned();
        files.push((alloc::format!("{name}/IntMetric0,ff6"), font.metrics()));
        files.push((alloc::format!("{name}/Outlines0,ff6"), font.outlines()));
    }
    files
}

/// Every point an element names.
fn points(element: &Element) -> impl Iterator<Item = Point> {
    let mut points = [None; 3];
    match *element {
        Element::Move(p) | Element::Line(p) => points[0] = Some(p),
        Element::Curve(a, b, c) => points = [Some(a), Some(b), Some(c)],
    }
    points.into_iter().flatten()
}

impl Character {
    /// Its box in design units: its points' extent, `x1, y1` past them.
    fn bbox(&self) -> [i32; 4] {
        let mut points = self.fill.iter().chain(&self.strokes).flat_map(points);
        let Some(first) = points.next() else {
            return [0; 4];
        };
        points.fold([first.x, first.y, first.x + 1, first.y + 1], |b, p| {
            [
                b[0].min(p.x),
                b[1].min(p.y),
                b[2].max(p.x + 1),
                b[3].max(p.y + 1),
            ]
        })
    }

    /// Whether its coordinates need twelve bits.
    fn wide(&self) -> bool {
        let [x0, y0, x1, y1] = self.bbox();
        let wide = |v: i32| !(-128..=127).contains(&v);
        self.fill
            .iter()
            .chain(&self.strokes)
            .flat_map(points)
            .any(|p| wide(p.x) || wide(p.y))
            || [x0, y0, x1 - x0, y1 - y0].into_iter().any(wide)
            || self.inclusions.iter().any(|(_, p)| wide(p.x) || wide(p.y))
    }
}

impl Font {
    /// Its metrics file: version 2, a map, boxes and x offsets in
    /// thousandths of an em, then its kern pairs.
    #[must_use]
    pub fn metrics(&self) -> Vec<u8> {
        let slots = self.characters.len() + 1;
        let scale = |v: i32| v * 1000 / i32::from(self.design_size);
        let mut out = self.name.clone();
        out.resize(40, b'\r');
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        let low = u8::try_from(slots & 0xFF).unwrap_or(0);
        let more = if self.kerns.is_empty() { 0 } else { 1 << 3 };
        out.extend_from_slice(&[low, 2, more | 1 << 2, 0]);
        let mut map = [0u8; 256];
        for (slot, &code) in self.characters.keys().enumerate() {
            map[usize::from(code)] = u8::try_from(slot + 1).unwrap_or(0);
        }
        out.extend_from_slice(&map);
        for corner in 0..4 {
            out.extend_from_slice(&0i16.to_le_bytes());
            for character in self.characters.values() {
                let value = i16::try_from(scale(character.bbox()[corner])).unwrap_or(0);
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out.extend_from_slice(&0i16.to_le_bytes());
        for character in self.characters.values() {
            out.extend_from_slice(&character.advance.to_le_bytes());
        }
        if !self.kerns.is_empty() {
            let area = 8u16;
            let mut kerns = Vec::new();
            let mut lefts: Vec<u8> = self.kerns.iter().map(|k| k.0).collect();
            lefts.dedup();
            for left in lefts {
                kerns.push(left);
                for &(_, right, x) in self.kerns.iter().filter(|k| k.0 == left) {
                    kerns.push(right);
                    kerns.extend_from_slice(&x.to_le_bytes());
                }
                kerns.push(0);
            }
            kerns.push(0);
            let end = area + u16::try_from(kerns.len()).unwrap_or(0);
            for offset in [area, area, end, end] {
                out.extend_from_slice(&offset.to_le_bytes());
            }
            out.extend_from_slice(&kerns);
        }
        out
    }

    /// Its outlines file: version 8, a chunk for every 32 codes up to its
    /// last, each character's data after its chunk's index.
    #[must_use]
    pub fn outlines(&self) -> Vec<u8> {
        let last = self.characters.keys().next_back().copied().unwrap_or(0);
        let chunks = usize::from(last) / CHUNK + 1;
        let mut out = b"FONT".to_vec();
        out.extend_from_slice(&[0, 8]);
        out.extend_from_slice(&self.design_size.to_le_bytes());
        let union = self
            .characters
            .values()
            .map(Character::bbox)
            .fold([0, 0, 0, 0], |b, c| {
                [
                    b[0].min(c[0]),
                    b[1].min(c[1]),
                    b[2].max(c[2]),
                    b[3].max(c[3]),
                ]
            });
        for value in [union[0], union[1], union[2] - union[0], union[3] - union[1]] {
            out.extend_from_slice(&i16::try_from(value).unwrap_or(0).to_le_bytes());
        }
        let offsets_at = 52 + 4;
        out.extend_from_slice(&u32::try_from(offsets_at).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&u32::try_from(chunks).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&[0; 20]);
        out.extend_from_slice(&TABLE_BYTES.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        let mut chunk_data = Vec::new();
        let mut starts = Vec::new();
        let table_end = offsets_at + 4 * (chunks + 1);
        for chunk in 0..chunks {
            starts.push(table_end + chunk_data.len());
            let mut index = Vec::new();
            let mut body = Vec::new();
            for i in 0..CHUNK {
                let code = u8::try_from(chunk * CHUNK + i).unwrap_or(0);
                match self.characters.get(&code) {
                    Some(character) if code != 0 => {
                        let offset = 4 * CHUNK + body.len();
                        index.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_le_bytes());
                        body.extend_from_slice(&encode(character));
                    }
                    _ => index.extend_from_slice(&0u32.to_le_bytes()),
                }
            }
            chunk_data.extend_from_slice(&(1u32 << 31).to_le_bytes());
            chunk_data.extend_from_slice(&index);
            chunk_data.extend_from_slice(&body);
            while chunk_data.len() % 4 != 0 {
                chunk_data.push(0);
            }
        }
        starts.push(table_end + chunk_data.len());
        for start in starts {
            out.extend_from_slice(&u32::try_from(start).unwrap_or(0).to_le_bytes());
        }
        out.extend_from_slice(&chunk_data);
        out
    }
}

/// A character's data: its flags, box, outline, strokes and inclusions.
fn encode(character: &Character) -> Vec<u8> {
    let wide = character.wide();
    let mut out = alloc::vec![(1 << 3) | u8::from(wide)];
    let point = |out: &mut Vec<u8>, p: Point| {
        if wide {
            let packed = (p.x.cast_unsigned() & 0xFFF) | ((p.y.cast_unsigned() & 0xFFF) << 12);
            out.extend_from_slice(&packed.to_le_bytes()[..3]);
        } else {
            out.extend_from_slice(&[p.x.to_le_bytes()[0], p.y.to_le_bytes()[0]]);
        }
    };
    let [x0, y0, x1, y1] = character.bbox();
    point(&mut out, Point { x: x0, y: y0 });
    point(
        &mut out,
        Point {
            x: x1 - x0,
            y: y1 - y0,
        },
    );
    let path = |out: &mut Vec<u8>, elements: &[Element]| {
        for element in elements {
            match *element {
                Element::Move(p) => {
                    out.push(1);
                    point(out, p);
                }
                Element::Line(p) => {
                    out.push(2);
                    point(out, p);
                }
                Element::Curve(a, b, c) => {
                    out.push(3);
                    for p in [a, b, c] {
                        point(out, p);
                    }
                }
            }
        }
    };
    path(&mut out, &character.fill);
    let mut terminator = 0;
    if !character.strokes.is_empty() {
        terminator |= 1 << 2;
    }
    if character.strokes.is_empty() && !character.inclusions.is_empty() {
        terminator |= 1 << 3;
    }
    out.push(terminator);
    if !character.strokes.is_empty() {
        path(&mut out, &character.strokes);
        out.push(if character.inclusions.is_empty() {
            0
        } else {
            1 << 3
        });
    }
    if !character.inclusions.is_empty() {
        for &(code, offset) in &character.inclusions {
            out.push(code);
            point(&mut out, offset);
        }
        out.push(0);
    }
    out
}

/// A glyph given pixel by pixel: its code, its bottom-left pixel, its width
/// and its levels from the top row down, each 0 to 15.
#[derive(Clone, Debug)]
pub struct Pixels {
    /// The code it is held at.
    pub code: u8,
    /// Its bottom-left pixel from the pen.
    pub corner: (i32, i32),
    /// Its pixels a row.
    pub width: usize,
    /// Its levels, the top row first.
    pub levels: Vec<u8>,
}

/// The `f9999x9999` file, where `four`, else the `b9999x9999` one, of font
/// `name` at `points` sixteenths of a point both ways and `dots` an inch,
/// holding `glyphs`, in the layout RISC OS 5.30's `Font_MakeBitmap` writes;
/// none where they do not fit the format.
#[must_use]
pub fn bitmap_file(
    name: &[u8],
    four: bool,
    points: i32,
    dots: i32,
    glyphs: &[Pixels],
) -> Option<Vec<u8>> {
    let size = crate::makebitmap::Size {
        depth: if four {
            crate::outlines::Depth::Four
        } else {
            crate::outlines::Depth::One
        },
        points: (points, points),
        resolution: (dots, dots),
        placements: (false, false),
    };
    let drawn = glyphs.iter().map(|glyph| {
        let rows = i32::try_from(glyph.levels.len() / glyph.width.max(1)).unwrap_or(0);
        Ok(crate::makebitmap::GlyphPixels {
            glyph: u32::from(glyph.code),
            placement: (0, 0),
            bitmap: crate::glyphs::Bitmap {
                left: glyph.corner.0,
                top: glyph.corner.1 + rows - 1,
                width: glyph.width,
                levels: glyph.levels.clone(),
            },
        })
    });
    crate::makebitmap::laid_out(name, &size, drawn).ok()
}

/// An `x90y45` file of one size, `points` at `dots` across and up, holding
/// `glyph`, its levels from its bottom row up.
#[must_use]
pub fn old_bitmaps(points: u8, dots: (u8, u8), glyph: &Pixels) -> Vec<u8> {
    let width = u8::try_from(glyph.width).unwrap_or(u8::MAX);
    let rows = u8::try_from(glyph.levels.len() / glyph.width.max(1)).unwrap_or(u8::MAX);
    let sixteenths = u32::from(points) * 16;
    let mut block = Vec::new();
    for word in [
        sixteenths * u32::from(dots.0),
        sixteenths * u32::from(dots.1),
        dots.0.into(),
        dots.1.into(),
    ] {
        block.extend_from_slice(&word.to_le_bytes());
    }
    let corner = |v: i32| i8::try_from(v).unwrap_or(0).cast_unsigned();
    let [x0, y0] = [corner(glyph.corner.0), corner(glyph.corner.1)];
    block.extend_from_slice(&[x0, y0, x0.wrapping_add(width), y0.wrapping_add(rows)]);
    let mut table = alloc::vec![0u8; 512];
    let at = 2 * usize::from(glyph.code);
    table[at..at + 2].copy_from_slice(&512u16.to_le_bytes());
    block.extend_from_slice(&table);
    block.extend_from_slice(&[x0, y0, width, rows]);
    let upward: Vec<u8> = glyph
        .levels
        .chunks(glyph.width.max(1))
        .rev()
        .flatten()
        .copied()
        .collect();
    for pair in upward.chunks(2) {
        block.push(pair[0] | pair.get(1).copied().unwrap_or(0) << 4);
    }
    block.resize(block.len().next_multiple_of(4), 0);
    let mut out = alloc::vec![0u8; 32];
    out[..4].copy_from_slice(&[points, 4, dots.0, dots.1]);
    out[8..12].copy_from_slice(&32u32.to_le_bytes());
    out[12..16].copy_from_slice(&u32::try_from(block.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&block);
    out
}

/// A font service for the tests: each character of a face a 500 by 700 unit
/// square of a 1000-unit em, advancing 600, the face's ascender 800 and
/// descender 200 of them; at most `answers` characters a request, counting
/// the requests, the characters asked and the families asked for.
#[derive(Debug)]
pub struct SquareFaces {
    /// The most characters a reply answers.
    pub answers: usize,
    /// The requests it was sent.
    pub requests: usize,
    /// The characters it answered, in order.
    pub asked: Vec<char>,
    /// The family each request asked for.
    pub families: Vec<tairix_abi::font_ipc::FamilyKey>,
}

impl SquareFaces {
    /// One answering at most `answers` characters a request.
    #[must_use]
    pub const fn capped(answers: usize) -> Self {
        Self {
            answers,
            requests: 0,
            asked: Vec::new(),
            families: Vec::new(),
        }
    }

    /// Its answer to `request`, appended to `reply`: whether it answered.
    pub fn answer(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        use tairix_abi::font_ipc::{
            ContourSource, FontRequest, FontUnits, GlyphSegment, OutlineBatchWriter, OutlineSource,
            Synthesis, FONT_MAX_OUTLINE_REPLY,
        };
        self.requests += 1;
        let Ok(FontRequest::Outlines {
            family, scalars, ..
        }) = FontRequest::from_bytes(request)
        else {
            return false;
        };
        self.families.push(family);
        let unit = |v: i32| FontUnits::from_raw(v * FontUnits::ONE);
        let segments = [
            GlyphSegment::Line {
                to: (unit(500), unit(0)),
            },
            GlyphSegment::Line {
                to: (unit(500), unit(700)),
            },
            GlyphSegment::Line {
                to: (unit(0), unit(700)),
            },
        ];
        let contour = [ContourSource {
            start: (unit(0), unit(0)),
            segments: &segments,
        }];
        let mut buffer = alloc::vec![0; FONT_MAX_OUTLINE_REPLY];
        let Ok(mut writer) = OutlineBatchWriter::new(&mut buffer, 1000, 800, 200, 0) else {
            return false;
        };
        for &character in scalars.scalars().iter().take(self.answers) {
            let source = OutlineSource {
                units_per_em: 1000,
                advance: unit(600),
                synth: Synthesis::NONE,
                contours: &contour,
            };
            if !matches!(writer.push(&source), Ok(true)) {
                break;
            }
            self.asked.push(character);
        }
        let Ok(length) = writer.finish() else {
            return false;
        };
        reply.extend_from_slice(&buffer[..length]);
        true
    }
}

/// [`SquareFaces`]'s answer to `request`, every character it asks for
/// answered: a font service a session serves.
#[must_use]
pub fn square_faces(request: &[u8], reply: &mut Vec<u8>) -> bool {
    SquareFaces::capped(usize::MAX).answer(request, reply)
}
