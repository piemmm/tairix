//! Deterministic fuzz harness for the sandbox seam's decode, helpdoc,
//! imagerender, and timesync surfaces.
//!
//! Two hostile directions, both driven through the public client path so
//! the request encoder, the service's request decoder, the decoders
//! themselves, and the caller-side reply validation are all exercised
//! together:
//!
//! * **Hostile input files** — mutated container/help-document/icon
//!   templates and pure noise fed to
//!   [`tairix_sandbox::decode::container_summary`] / [`manifest_summary`] /
//!   [`disassemble`] / [`render_help`] / [`rasterise_icon`] over the
//!   in-process loopback worker — and mutated NTP server replies fed to
//!   [`tairix_sandbox::timesync::evaluate_datagram`]: every outcome must be
//!   a typed result, never a panic.
//! * **Hostile workers** — a launcher whose "worker" frames pure noise as
//!   its reply: the caller-side fail-closed reply decoders must refuse or
//!   accept typed, never panic, and the seam must survive.
//! * **Hostile session workers** — the duplex seam's inbound codec fed the
//!   same noise a byte-run at a time through `on_readable`/`recv`: every
//!   outcome must be a typed result, no frame may escape the session's own
//!   inbound ceiling, and a contained session must stay contained.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` drives
//! the mutations through the shared `tairix_fuzzseed` seam. A plain
//! `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh,
//! logged seed; `cargo xtask fuzz` exports `TAIRIX_FUZZ_BUDGET_SECS` to
//! extend the loop to a wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_image::{
    encode_bmp, encode_gif, encode_png, encode_sprite_area, encode_tiff, Density, DensityUnit,
    GifOptions, IndexDepth, Picture, PictureSource, SpriteInput, SpriteMode, SpriteName,
    SpritePalette, TiffOptions,
};
use tairix_raster::Region;
use tairix_sandbox::decode::{
    container_summary, disassemble, manifest_summary, DecodeService, Isa,
};
use tairix_sandbox::helpdoc::{render_help, HelpService, RenderMode, Styling};
use tairix_sandbox::host::{Launcher, ParserSandbox};
use tairix_sandbox::imageedit::{
    close_edit, open_edit, read_kept, read_rows, select_entry, EditDocument, EditEntry, EditKept,
    EditKind, EditPicture, EditPixels, KeptReason,
};
use tairix_sandbox::imagerender::{
    close_view, open_view, plan_wallpaper, rasterise_icon, render_page, render_thumbnail,
    select_page, send_document, ImageRenderService, ViewFormat, WallpaperRenderFailure,
    MAX_DESTINATION_WIDTH, MAX_ICON_SIDE,
};
use tairix_sandbox::loopback::{LoopbackLauncher, LoopbackSession};
use tairix_sandbox::proto::Channel;
use tairix_sandbox::session::{
    FrameOut, SandboxSession, SessionBounds, SessionDescriptors, SessionService, SessionStep,
    SessionTransport, MIN_QUEUE_BYTES,
};
use tairix_sandbox::textsyntax::{detect, lex_lines, validate_document, TextSyntaxService};
use tairix_sandbox::timesync::{evaluate_datagram, TimeSyncService};
use tairix_sandbox::worker::Service;
use tairix_svg::font::NoFonts;
use tairix_syntax::{Format, LineState};
use tairix_wallpaper::WallpaperFit;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 2_000;

/// Largest arbitrary byte string fed as an input file or a hostile reply.
const MAX_NOISE: usize = 2048;

/// Draw `image` under `fit` onto a `width`×`height` destination the way the
/// desktop draws a wallpaper: upload, plan, render.
fn drawn<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    width: u32,
    height: u32,
    fit: WallpaperFit,
    image: &[u8],
) -> Result<Vec<u8>, WallpaperRenderFailure> {
    send_document(sandbox, image).map_err(WallpaperRenderFailure::Document)?;
    let planned = plan_wallpaper(sandbox, (width, height), width, height, fit)?;
    let mut out = vec![0u8; width as usize * height as usize * 4];
    planned.render_into(&mut out)?;
    Ok(out)
}

/// A well-formed stratum-2 NTP server reply echoing `nonce`, reporting an
/// instant inside the plausibility window — the template mutations start from.
fn ntp_reply_template(nonce: u64) -> Vec<u8> {
    /// Seconds from the NTP epoch (1900) to the Unix epoch (1970).
    const NTP_UNIX_DELTA: i64 = 2_208_988_800;
    let unix_secs = tairix_abi::RELEASE_EPOCH_SECS + 86_400;
    let field = u32::try_from((unix_secs + NTP_UNIX_DELTA).rem_euclid(1 << 32))
        .expect("reduced modulo 2^32");
    let ts = (u64::from(field) << 32).to_be_bytes();
    let mut p = vec![0u8; tairix_net::ntp::PACKET_LEN];
    p[0] = (4 << 3) | 4; // version 4, mode 4 (server)
    p[1] = 2; // stratum
    p[24..32].copy_from_slice(&nonce.to_be_bytes());
    p[32..40].copy_from_slice(&ts);
    p[40..48].copy_from_slice(&ts);
    p
}

/// One help-render fuzz iteration: a help document with a handful of bytes
/// flipped, a random truncation, and pure noise, rendered through the honest
/// worker under every surface, styling level, and served-locale tag (including
/// malformed spellings, so the request grammar is fuzzed too).
///
/// Returns the render mode used, so the caller can reuse it against the
/// hostile worker.
fn fuzz_help_iteration(
    sandbox: &mut ParserSandbox<LoopbackLauncher<fn() -> HelpService>, SilentSink>,
    noise: &[u8],
    rng: &mut Prng,
) -> RenderMode {
    let mut help = HELP_TEMPLATE.to_vec();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(help.len());
        help[pos] ^= rng.next_u8();
    }
    let mode = if rng.next_u64() & 1 == 0 {
        RenderMode::Short
    } else {
        RenderMode::Full
    };
    let styling = match rng.at_most(2) {
        0 => Styling::Plain,
        1 => Styling::Monochrome,
        _ => Styling::Colour,
    };
    let locales = ["en-US", "fr-FR", "zh-CN", "", "not a tag", "xx-XX"];
    let locale = *rng.pick(&locales);
    let _ = render_help(sandbox, mode, styling, locale, &help);
    let cut = rng.at_most(help.len());
    let _ = render_help(sandbox, mode, styling, locale, &help[..cut]);
    let _ = render_help(sandbox, mode, styling, locale, noise);
    mode
}

/// One text-syntax fuzz iteration: `noise`, cut into lines, lexed from a
/// random state in a random format, validated as a store and sniffed as a
/// head. Whatever the caller believes must hold the span contract against
/// the lines it sent.
fn fuzz_text_iteration<L: Launcher>(
    sandbox: &mut ParserSandbox<L, SilentSink>,
    noise: &[u8],
    rng: &mut Prng,
) {
    let format = *rng.pick(&Format::ALL);
    let lines: Vec<&[u8]> = noise.split(|&b| b == b'\n').collect();
    let state = LineState::from_raw(rng.next_u32());
    if let Ok(batch) = lex_lines(sandbox, format, state, &lines) {
        assert_eq!(batch.lines.len(), lines.len());
        for (at, line) in lines.iter().enumerate() {
            let mut last = 0;
            for span in batch.line(at) {
                assert!(span.start >= last && span.start < span.end);
                assert!(span.end as usize <= line.len());
                last = span.end;
            }
        }
    }
    let line_count = lines.len();
    if let Ok(diagnostics) = validate_document(sandbox, format, noise) {
        for diagnostic in diagnostics {
            assert!(diagnostic.line.is_none_or(|at| at as usize <= line_count));
            assert!(!diagnostic.message.is_empty());
        }
    }
    let _ = detect(
        sandbox,
        &noise[..noise.len().min(tairix_sandbox::textsyntax::MAX_HEAD_LEN)],
    );
}

/// One NTP-evaluation fuzz iteration: a mutated well-formed reply, a
/// truncated one, and pure noise, each evaluated through the honest worker
/// under both a matching and a mismatched nonce. A sample that survives must
/// be one the engine's own rules admit — the caller-side re-validation the
/// authority split rests on.
///
/// Returns the nonce and receive instant used, so the caller can reuse them
/// against the hostile worker.
fn fuzz_ntp_iteration(
    sandbox: &mut ParserSandbox<LoopbackLauncher<fn() -> TimeSyncService>, SilentSink>,
    noise: &[u8],
    rng: &mut Prng,
) -> (u64, tairix_abi::time::Duration64) {
    let nonce = rng.next_u64();
    let mut packet = ntp_reply_template(nonce);
    for _ in 0..rng.at_most(8) {
        let pos = rng.below(packet.len());
        packet[pos] ^= rng.next_u8();
    }
    let received = tairix_abi::time::Duration64::from_nanos(rng.next_u64() >> 24);
    let cut = rng.at_most(packet.len());
    for (label_nonce, bytes) in [
        (nonce, &packet[..]),
        (nonce ^ 1, &packet[..]),
        (nonce, &packet[..cut]),
        (nonce, noise),
    ] {
        let txn = tairix_net::ntp::Transaction {
            server: 0,
            nonce: tairix_net::ntp::NtpTimestamp::from_raw(label_nonce),
            sent_at: tairix_abi::time::Duration64::ZERO,
        };
        if let Ok(tairix_net::ntp::Reply::Sample(sample)) =
            evaluate_datagram(sandbox, &txn, received, bytes)
        {
            assert!(
                tairix_abi::is_plausible_wall_time(sample.true_time),
                "an implausible instant escaped the evaluation"
            );
            assert!(
                sample.round_trip <= tairix_net::ntp::MAX_ROUND_TRIP,
                "an over-long round trip escaped the evaluation"
            );
            assert!(
                (1..16).contains(&sample.stratum),
                "an unusable stratum escaped the evaluation"
            );
        }
    }
    (nonce, received)
}

/// Discards every logged event.
struct SilentSink;

impl tairix_log::Sink for SilentSink {
    fn write_event(&self, _event: &tairix_log::Event<'_>) {}
}

/// A minimal valid wasm module with two function bodies.
fn wasm_template() -> Vec<u8> {
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    // code section: id 10, payload = count 2, two 3-byte bodies.
    let payload = [2u8, 3, 0, 1, 0x0B, 3, 0, 1, 0x0B];
    bytes.push(10);
    bytes.push(u8::try_from(payload.len()).expect("small"));
    bytes.extend_from_slice(&payload);
    bytes
}

/// A minimal valid rxe image (one RX segment, PIE, current ABI).
fn rxe_template() -> Vec<u8> {
    let segment = tairix_abi::Segment {
        vaddr: tairix_abi::RXE_PAGE_SIZE,
        file_offset: 0,
        file_size: 32,
        mem_size: tairix_abi::RXE_PAGE_SIZE,
        permission: tairix_abi::RxePermission::ReadExecute,
    };
    let header = tairix_abi::LoadHeader {
        magic: tairix_abi::LOAD_MAGIC,
        abi_version: tairix_abi::ABI_VERSION_CURRENT,
        flags: tairix_abi::LOAD_FLAG_PIE,
        segment_count: 1,
        needed_count: 0,
        entry: tairix_abi::RXE_PAGE_SIZE,
        cfi_tag: [0x5A; 32],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&header.to_le_bytes());
    bytes.extend_from_slice(&segment.to_le_bytes());
    bytes
}

/// The four request ISAs, cycled by the driver.
const ISAS: [Isa; 4] = [Isa::X86_64, Isa::Aarch64, Isa::Riscv64, Isa::Wasm];

/// A minimal valid help document, as the helpdoc mutation template.
const HELP_TEMPLATE: &[u8] =
    b"## NAME\n\ntop \xe2\x80\x94 display tasks\n\n## SYNOPSIS\n\n`top [-d seconds]`\n\n## DESCRIPTION\n\nShows tasks.\n";

/// A minimal valid SVG icon, as the imagerender mutation template.
const SVG_TEMPLATE: &[u8] =
    br##"<svg viewBox="0 0 10 10"><polygon points="0,0 10,0 10,10 0,10" fill="#3070f0"/></svg>"##;

fn chunk(chunk_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let len = u32::try_from(payload.len()).expect("test payload fits a u32 length");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&chunk_type);
    out.extend_from_slice(payload);
    let mut crc = tairix_crc32::Crc32::new();
    crc.update(&chunk_type);
    crc.update(payload);
    let crc = crc.finish();
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// The Adler-32 checksum RFC 1950 requires as the zlib stream trailer.
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Wrap `data` in a well-formed zlib stream built from a single STORED
/// deflate block, so no compressor is needed to produce a stream
/// `tairix_image`'s decoder accepts.
fn zlib_wrap(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78u8, 0x9C, 0x01];
    let len = u16::try_from(data.len()).expect("template fits a u16 length");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// The five wallpaper fits, cycled by the driver.
const FITS: [WallpaperFit; 5] = [
    WallpaperFit::Fill,
    WallpaperFit::Fit,
    WallpaperFit::Stretch,
    WallpaperFit::Centre,
    WallpaperFit::Tile,
];

/// A minimal valid 2x2 RGBA8 PNG icon, as both the imagerender and the
/// wallpaper mutation template.
fn png_template() -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    let raw = [
        0, 10, 20, 30, 255, 40, 50, 60, 255, // row 0: filter None, 2 pixels
        0, 70, 80, 90, 255, 100, 110, 120, 255, // row 1
    ];
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(chunk(*b"IHDR", &ihdr));
    bytes.extend(chunk(*b"IDAT", &zlib_wrap(&raw)));
    bytes.extend(chunk(*b"IEND", &[]));
    bytes
}

/// A "worker" that answers every request with seeded noise framed as a
/// reply — the compromised-worker model.
struct HostileChannel {
    pending: Vec<u8>,
    at: usize,
}

impl Channel for HostileChannel {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, tairix_abi::Errno> {
        if self.at == self.pending.len() || buf.is_empty() {
            return Ok(0);
        }
        let take = buf.len().min(self.pending.len() - self.at);
        buf[..take].copy_from_slice(&self.pending[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, tairix_abi::Errno> {
        Ok(buf.len())
    }
}

/// The honest icon-rasterisation sandbox the fuzz loop drives.
type HonestIconSandbox = ParserSandbox<LoopbackLauncher<fn() -> ImageRenderService>, SilentSink>;

/// Fuzz one iteration's icon coverage: an SVG icon and a PNG icon, each
/// with a handful of bytes flipped, a random truncation, and pure `noise`,
/// rasterised through the honest worker at a random side. Returns the side
/// used, so the caller can reuse it against the hostile worker too.
fn fuzz_icon_iteration(honest_icon: &mut HonestIconSandbox, noise: &[u8], rng: &mut Prng) -> u32 {
    let side = u32::try_from(rng.at_most(usize::try_from(MAX_ICON_SIDE - 1).unwrap_or(0)))
        .unwrap_or(0)
        + 1;
    let mut svg = SVG_TEMPLATE.to_vec();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(svg.len());
        svg[pos] ^= rng.next_u8();
    }
    let _ = rasterise_icon(honest_icon, side, &svg, &mut NoFonts);
    let cut = rng.at_most(svg.len());
    let _ = rasterise_icon(honest_icon, side, &svg[..cut], &mut NoFonts);
    let mut png = png_template();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(png.len());
        png[pos] ^= rng.next_u8();
    }
    let _ = rasterise_icon(honest_icon, side, &png, &mut NoFonts);
    let cut = rng.at_most(png.len());
    let _ = rasterise_icon(honest_icon, side, &png[..cut], &mut NoFonts);
    let _ = rasterise_icon(honest_icon, side, noise, &mut NoFonts);
    side
}

/// Fuzz one iteration's wallpaper coverage: a PNG source, mutated, a
/// random truncation, and pure `noise`, rendered through the honest worker
/// (the same one [`fuzz_icon_iteration`] drives, since one worker serves
/// both surfaces) at a random small destination and fit. Returns the
/// destination and fit used, so the caller can reuse them against the
/// hostile worker too.
fn fuzz_wallpaper_iteration(
    honest: &mut HonestIconSandbox,
    noise: &[u8],
    rng: &mut Prng,
) -> (u32, u32, WallpaperFit) {
    let width = u32::try_from(rng.at_most(15)).unwrap_or(0) + 1;
    let height = u32::try_from(rng.at_most(15)).unwrap_or(0) + 1;
    let fit = *rng.pick(&FITS);
    let mut png = png_template();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(png.len());
        png[pos] ^= rng.next_u8();
    }
    let _ = drawn(honest, width, height, fit, &png);
    let cut = rng.at_most(png.len());
    let _ = drawn(honest, width, height, fit, &png[..cut]);
    let _ = drawn(honest, width, height, fit, noise);
    // Also exercise a destination one past the ceiling: refused by the
    // plan's own check before anything is decoded, so it is cheap to run
    // every iteration unlike a genuine ceiling-sized render.
    let _ = drawn(honest, MAX_DESTINATION_WIDTH + 1, height, fit, &png);
    (width, height, fit)
}

/// Fuzz one iteration's document-viewing coverage: a PNG document,
/// mutated, a mutated drawing, a random truncation, and pure `noise`,
/// opened through the honest worker and driven page-by-page and
/// band-by-band at a random small window of a random magnification.
///
/// The view is a *session*, so what this reaches that the one-shot
/// surfaces cannot is the order requests arrive in: a render before a
/// page, a band before a render, a page past the count, a window that
/// wanders outside the extent it names. Every one of those must be a
/// typed refusal rather than anything else. Both backings are driven,
/// because a vector document reaches an entirely different draw path
/// behind the same request grammar.
fn fuzz_view_iteration(honest: &mut HonestIconSandbox, noise: &[u8], rng: &mut Prng) {
    let mut png = png_template();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(png.len());
        png[pos] ^= rng.next_u8();
    }
    let mut svg = SVG_TEMPLATE.to_vec();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(svg.len());
        svg[pos] ^= rng.next_u8();
    }
    let cut = rng.at_most(png.len());
    for document in [png.as_slice(), &png[..cut], svg.as_slice(), noise] {
        // A page and a band before anything is open, so the out-of-order
        // paths are reached whether or not this document opens at all.
        let _ = select_page(honest, u32::try_from(rng.at_most(4)).unwrap_or(0));
        let _ = render_page(honest, (1, 1), whole(1, 1), &mut [0u8; 4]);
        if send_document(honest, document).is_err() {
            continue;
        }
        let named = *rng.pick(&NAMED_FORMATS);
        let Ok(opened) = open_view(honest, named, &mut NoFonts) else {
            continue;
        };
        let index = u32::try_from(rng.at_most(4)).unwrap_or(0);
        let Ok(page) = select_page(honest, index % opened.count.max(1)) else {
            continue;
        };
        let width = u32::try_from(rng.at_most(7)).unwrap_or(0) + 1;
        let height = u32::try_from(rng.at_most(7)).unwrap_or(0) + 1;
        let window = Region {
            x: u32::try_from(rng.at_most(3)).unwrap_or(0),
            y: u32::try_from(rng.at_most(3)).unwrap_or(0),
            width,
            height,
        };
        let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
        // Deliberately not clamped to the magnification: a window that
        // runs off it must be refused, never drawn from pixels that are
        // not there.
        let _ = render_page(
            honest,
            (
                u32::try_from(rng.at_most(15)).unwrap_or(0) + 1,
                u32::try_from(rng.at_most(15)).unwrap_or(0) + 1,
            ),
            window,
            &mut out,
        );
        // And one that certainly does fit, at the page's own scale, so a
        // successful draw is reached as well as the refusals.
        let mut whole_out = vec![0u8; (page.width as usize) * (page.height as usize) * 4];
        let _ = render_page(
            honest,
            (page.width, page.height),
            whole(page.width, page.height),
            &mut whole_out,
        );
        let _ = close_view(honest);
    }
}

/// Mutated pictures, a truncation and noise, each drawn as a thumbnail at a
/// random side and named format: every reply is either a refusal or exactly
/// the square asked for.
fn fuzz_thumbnail_iteration(honest: &mut HonestIconSandbox, noise: &[u8], rng: &mut Prng) {
    let mut png = png_template();
    for _ in 0..rng.at_most(6) {
        let pos = rng.below(png.len());
        png[pos] ^= rng.next_u8();
    }
    let cut = rng.at_most(png.len());
    for document in [png.as_slice(), &png[..cut], noise] {
        let side = u32::try_from(rng.at_most(64)).unwrap_or(0) + 1;
        // Drawn before anything is uploaded too, so the no-document refusal is
        // reached whether or not the upload below succeeds.
        let _ = render_thumbnail(honest, side, None);
        if send_document(honest, document).is_err() {
            continue;
        }
        if let Ok(fitted) = render_thumbnail(honest, side, *rng.pick(&NAMED_FORMATS)) {
            assert_fitted(&fitted, side);
        }
    }
}

/// A believed thumbnail is exactly the square asked for, with its picture
/// placed inside it.
fn assert_fitted(fitted: &tairix_icon::Fitted, side: u32) {
    assert_eq!(fitted.pixels.len(), (side as usize) * (side as usize) * 4);
    let square = tairix_geometry::Rect::new(0, 0, side, side);
    assert!(!fitted.bounds.is_empty());
    assert_eq!(fitted.bounds.intersection(&square), fitted.bounds);
}

/// A small paletted PNG and a sprite area of a masked paletted sprite and a
/// truecolour one: the documents the edit leg mutates.
fn edit_templates() -> [Vec<u8>; 6] {
    let palette: Vec<[u8; 4]> = (0..16u8).map(|i| [i * 16, 255 - i, i, 255]).collect();
    let indices: Vec<u8> = (0..12).map(|i| i % 16).collect();
    let paletted = Picture::indexed(
        4,
        3,
        IndexDepth::Four,
        palette,
        indices,
        Some((0..12).map(|i| if i % 3 == 0 { 0 } else { 255 }).collect()),
    )
    .unwrap_or_else(|_| unreachable!("a valid fixture"));
    let unmasked = Picture::indexed(
        4,
        3,
        IndexDepth::Four,
        (0..16u8).map(|i| [i, i, i, 255]).collect(),
        (0..12).collect(),
        None,
    )
    .unwrap_or_else(|_| unreachable!("a valid fixture"));
    let truecolour = Picture::rgba(2, 2, (0..16u8).collect())
        .unwrap_or_else(|_| unreachable!("a valid fixture"));
    let png = encode_png(&unmasked).unwrap_or_default();
    let area = encode_sprite_area(&[
        SpriteInput::Picture {
            name: SpriteName::new("icon").unwrap_or_else(|| unreachable!("a valid name")),
            mode: SpriteMode::indexed(IndexDepth::Four, (1, 1), false),
            palette: &SpritePalette::Full,
            masked: true,
            source: &paletted,
        },
        SpriteInput::Picture {
            name: SpriteName::new("photo").unwrap_or_else(|| unreachable!("a valid name")),
            mode: SpriteMode::truecolour((1, 1), true),
            palette: &SpritePalette::Implied,
            masked: true,
            source: &truecolour,
        },
    ])
    .unwrap_or_default();
    let dense = truecolour
        .clone()
        .with_density(Density::whole(300, 150, DensityUnit::Inch));
    let gif = encode_gif(&unmasked, GifOptions { interlaced: true }).unwrap_or_default();
    let bmp = encode_bmp(&dense).unwrap_or_default();
    let pages: [&dyn PictureSource; 3] = [&paletted, &unmasked, &dense];
    let tiff = encode_tiff(&pages, TiffOptions::default()).unwrap_or_default();
    let layer = tairix_image::OraLayerSource {
        name: "layer",
        picture: &truecolour,
        at: (0, 0),
        opacity: 200,
        visible: true,
    };
    let ora =
        tairix_image::encode_ora((2, 2), &[layer], &truecolour, &truecolour).unwrap_or_default();
    [png, area, gif, bmp, tiff, ora]
}

/// The formats an edit open may be asked to read a document as.
const EDIT_FORMATS: [Option<ViewFormat>; 8] = [
    None,
    Some(ViewFormat::Png),
    Some(ViewFormat::Sprite),
    Some(ViewFormat::Gif),
    Some(ViewFormat::Bmp),
    Some(ViewFormat::Tiff),
    Some(ViewFormat::Svg),
    Some(ViewFormat::OpenRaster),
];

/// Fuzz one iteration of the edit decode: out-of-order requests first, then
/// a mutated paletted PNG, sprite area, GIF, BMP, TIFF and OpenRaster
/// document, their truncations and `noise`,
/// each opened, every entry selected, and every row or kept byte fetched.
/// A row handed on must be exactly the width its description promised.
fn fuzz_edit_iteration(honest: &mut HonestIconSandbox, noise: &[u8], rng: &mut Prng) {
    let _ = select_entry(honest, stray_document(), 0);
    let _ = read_rows(honest, &stray_picture(), |_, _, _| {});
    for template in edit_templates() {
        let mut mutated = template;
        for _ in 0..rng.at_most(4) {
            if !mutated.is_empty() {
                let pos = rng.below(mutated.len());
                mutated[pos] ^= rng.next_u8();
            }
        }
        let cut = rng.at_most(mutated.len());
        for document in [mutated.as_slice(), &mutated[..cut], noise] {
            if send_document(honest, document).is_err() {
                continue;
            }
            let Ok(opened) = open_edit(honest, *rng.pick(&EDIT_FORMATS)) else {
                continue;
            };
            read_every_entry(honest, opened);
            let _ = close_edit(honest);
        }
    }
}

/// Fetch the rows or bytes of up to four entries of `opened`, holding every
/// row handed on to the width its description promised and every index to
/// its palette.
fn read_every_entry<L: Launcher>(sandbox: &mut ParserSandbox<L, SilentSink>, opened: EditDocument) {
    for index in 0..opened.count.min(4) {
        match select_entry(sandbox, opened, index) {
            Ok(EditEntry::Picture(picture)) => {
                let width = picture.width() as usize;
                let sample = picture.pixels().sample_bytes();
                let plane = picture.pixels().has_plane();
                let colours = match picture.pixels() {
                    EditPixels::Indexed { palette, .. } => palette.len(),
                    EditPixels::Rgba => usize::MAX,
                };
                let _ = read_rows(sandbox, &picture, |_, samples, alpha| {
                    assert_eq!(samples.len(), width * sample);
                    assert_eq!(alpha.len(), if plane { width } else { 0 });
                    if colours != usize::MAX {
                        assert!(samples.iter().all(|&index| usize::from(index) < colours));
                    }
                });
            }
            Ok(EditEntry::Kept(kept)) => {
                let mut out = Vec::new();
                if read_kept(sandbox, &kept, &mut out).is_ok() {
                    assert_eq!(out.len(), kept.length as usize);
                }
            }
            Err(_) => {}
        }
    }
}

/// Drive every document request — the upload, the view and the edit
/// decode — against a worker whose every reply is framed noise. Each
/// request is asked of the worker as it stands, so every reply decoder
/// meets noise rather than only the first.
fn hostile_document_iteration<L: Launcher>(hostile: &mut ParserSandbox<L, SilentSink>) {
    let _ = send_document(hostile, &png_template());
    let _ = open_view(hostile, None, &mut NoFonts);
    let _ = select_page(hostile, 0);
    let _ = render_page(hostile, (1, 1), whole(1, 1), &mut [0u8; 4]);
    let _ = close_view(hostile);
    let _ = open_edit(hostile, None);
    let _ = select_entry(hostile, stray_document(), 0);
    let _ = read_rows(hostile, &stray_picture(), |_, _, _| {});
    let kept = EditKept {
        name: SpriteName::new("stray").unwrap_or_else(|| unreachable!("a valid name")),
        reason: KeptReason::Damaged,
        length: 48,
    };
    let _ = read_kept(hostile, &kept, &mut Vec::new());
    let _ = close_edit(hostile);
}

/// A picture no worker described, for requests out of order and for the
/// hostile worker to answer about.
fn stray_picture() -> EditPicture {
    let pixels = EditPixels::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0; 4]; 4],
        plane: true,
    };
    EditPicture::new(2, 2, pixels, None).unwrap_or_else(|| unreachable!("within the bounds"))
}

/// A document no worker opened, for the same.
const fn stray_document() -> EditDocument {
    EditDocument {
        format: ViewFormat::Sprite,
        kind: EditKind::Sprites,
        count: 1,
        unkept: tairix_image::Unkept {
            precision: false,
            extras: false,
            converted: false,
        },
        written: tairix_image::Written::Plain,
        canvas: None,
    }
}

/// The whole of a `width`×`height` picture.
fn whole(width: u32, height: u32) -> Region {
    Region {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// The formats a view open may be asked to read a document as, including
/// reading its own signature.
const NAMED_FORMATS: [Option<ViewFormat>; 5] = [
    None,
    Some(ViewFormat::Png),
    Some(ViewFormat::Sprite),
    Some(ViewFormat::Tiff),
    Some(ViewFormat::Svg),
];

/// A session "worker" whose whole reply stream is seeded noise, delivered
/// `chunk` bytes at a time so the accumulator's partial-frame paths are
/// reached as well as its whole-frame ones.
struct HostileSession {
    stream: Vec<u8>,
    at: usize,
    chunk: usize,
}

impl SessionTransport for HostileSession {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, tairix_abi::Errno> {
        let take = buf.len().min(self.chunk).min(self.stream.len() - self.at);
        buf[..take].copy_from_slice(&self.stream[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, tairix_abi::Errno> {
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

/// Answers each inbound frame with a random number of outbound ones, so
/// the honest leg exercises the encoder, the fan-out, and the inbound
/// accumulator's back-pressure together.
struct FanService {
    fan: usize,
}

impl SessionService for FanService {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        for _ in 0..self.fan {
            if out.frame(request).is_err() {
                break;
            }
        }
        SessionStep::Continue
    }
}

/// One session fuzz iteration.
///
/// The hostile leg drives the inbound codec over pure noise: every frame
/// that escapes must be one the session's own ceiling admits, and a
/// contained session must refuse everything afterwards rather than
/// half-working. The honest leg round-trips a random payload through the
/// in-process fake, so the outbound encoder and the queue bounds are
/// fuzzed alongside the decoder.
fn fuzz_session_iteration(noise: &[u8], rng: &mut Prng) {
    let outbound = MIN_QUEUE_BYTES + rng.at_most(512);
    let inbound = MIN_QUEUE_BYTES + rng.at_most(512);
    let bounds = SessionBounds::new(outbound, inbound).expect("above the floor");
    let chunk = 1 + rng.at_most(63);

    let mut hostile = SandboxSession::new(
        HostileSession {
            stream: noise.to_vec(),
            at: 0,
            chunk,
        },
        bounds,
        SilentSink,
    )
    .expect("the bounds commit");
    // A payload straddling the send ceiling: refused typed either way,
    // and never half-queued.
    let payload = vec![0xA5u8; rng.at_most(outbound + 16)];
    let _ = hostile.send(&payload);
    let _ = hostile.on_writable();
    let mut contained = false;
    // Exactly enough turns to consume the whole stream plus its end.
    for _ in 0..noise.len() / chunk + 4 {
        if hostile.on_readable().is_err() {
            contained = true;
            break;
        }
        loop {
            match hostile.recv(<[u8]>::len) {
                Ok(Some(len)) => assert!(
                    len <= bounds.max_recv_payload(),
                    "a frame above the inbound ceiling escaped the session"
                ),
                Ok(None) => break,
                Err(_) => {
                    contained = true;
                    break;
                }
            }
        }
        if contained || hostile.peer_finished() {
            break;
        }
    }
    if contained {
        // A contained session stays contained on every surface.
        assert!(hostile.send(b"x").is_err());
        assert!(hostile.recv(<[u8]>::len).is_err());
        assert!(!hostile.wants_read());
        assert!(!hostile.wants_write());
    }

    let fan = 1 + rng.at_most(3);
    let mut honest =
        SandboxSession::new(LoopbackSession::new(FanService { fan }), bounds, SilentSink)
            .expect("the bounds commit");
    let payload = vec![0x5Au8; rng.at_most(bounds.max_send_payload())];
    if honest.send(&payload).is_ok() {
        while honest.wants_write() && honest.on_writable().is_ok() {}
        let mut seen = 0;
        while honest.wants_read() && honest.on_readable().is_ok() {
            let mut drained = false;
            while let Ok(Some(exact)) = honest.recv(|frame| frame == payload.as_slice()) {
                assert!(exact, "the honest round trip is byte-exact");
                seen += 1;
                drained = true;
            }
            if !drained {
                break;
            }
        }
        assert!(
            seen <= fan,
            "more frames came back than the service emitted"
        );
    }
}

/// Launches [`HostileChannel`] workers with fresh noise per launch.
struct HostileLauncher {
    rng: Prng,
}

impl Launcher for HostileLauncher {
    type Channel = HostileChannel;

    fn launch(&mut self) -> Result<HostileChannel, tairix_abi::Errno> {
        let rng = &mut self.rng;
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        let mut pending = u32::try_from(noise.len())
            .expect("bounded noise")
            .to_le_bytes()
            .to_vec();
        pending.extend_from_slice(&noise);
        Ok(HostileChannel { pending, at: 0 })
    }

    fn dispose(&mut self, _channel: HostileChannel) -> Option<i32> {
        None
    }
}

#[test]
fn decode_surface_never_panics_for_any_input_or_reply() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "decode_surface_never_panics_for_any_input_or_reply",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));

    let mut honest = ParserSandbox::new(
        LoopbackLauncher::new(DecodeService::default as fn() -> DecodeService),
        SilentSink,
    );
    let mut honest_help = ParserSandbox::new(
        LoopbackLauncher::new(HelpService::default as fn() -> HelpService),
        SilentSink,
    );
    let mut honest_icon = ParserSandbox::new(
        LoopbackLauncher::new(ImageRenderService::default as fn() -> ImageRenderService),
        SilentSink,
    );
    let mut honest_time = ParserSandbox::new(
        LoopbackLauncher::new(TimeSyncService::default as fn() -> TimeSyncService),
        SilentSink,
    );
    let mut honest_text = ParserSandbox::new(
        LoopbackLauncher::new(TextSyntaxService::default as fn() -> TextSyntaxService),
        SilentSink,
    );
    let mut hostile = ParserSandbox::new(
        HostileLauncher {
            rng: Prng::new(rng.next_u64()),
        },
        SilentSink,
    );
    let wasm = wasm_template();
    let rxe = rxe_template();

    let mut iteration: u64 = 0;
    loop {
        // 1. A container template with a handful of bytes flipped, plus a
        //    random truncation, summarised through the honest worker.
        let template = if rng.next_u64() & 1 == 0 { &wasm } else { &rxe };
        let mut mutated = template.clone();
        for _ in 0..rng.at_most(6) {
            let pos = rng.below(mutated.len());
            mutated[pos] ^= rng.next_u8();
        }
        let _ = container_summary(&mut honest, &mutated);
        let cut = rng.at_most(mutated.len());
        let _ = container_summary(&mut honest, &mutated[..cut]);

        // 2. Pure noise as an input file and as a manifest.
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        let _ = container_summary(&mut honest, &noise);
        let _ = manifest_summary(&mut honest, &noise);

        // 3. Noise disassembled under every ISA at a random address,
        //    depth, and window size.
        let isa = *rng.pick(&ISAS);
        let _ = disassemble(
            &mut honest,
            isa,
            rng.next_u64(),
            u32::from(rng.next_u8()),
            u32::from(rng.next_u8()),
            &noise,
        );

        // 4. Help documents through the honest worker, in their own helper
        //    to keep this loop's body a readable, bounded size. Returns the
        //    render mode used, so the caller can reuse it against the
        //    hostile worker.
        let mode = fuzz_help_iteration(&mut honest_help, &noise, &mut rng);

        // 6. The icon-rasterisation and wallpaper surfaces, fuzzed in
        //    their own helpers to keep this loop's body a readable,
        //    bounded size. Both run over the same worker instance,
        //    exercising the two surfaces interleaved.
        let side = fuzz_icon_iteration(&mut honest_icon, &noise, &mut rng);
        let (wallpaper_w, wallpaper_h, fit) =
            fuzz_wallpaper_iteration(&mut honest_icon, &noise, &mut rng);
        fuzz_view_iteration(&mut honest_icon, &noise, &mut rng);
        fuzz_thumbnail_iteration(&mut honest_icon, &noise, &mut rng);
        fuzz_edit_iteration(&mut honest_icon, &noise, &mut rng);

        // 6b. The duplex session seam: its inbound codec over the same
        //    noise, plus an honest round trip through the in-process fake.
        fuzz_session_iteration(&noise, &mut rng);

        // 7. NTP server replies through the honest worker, in its own helper
        //    to keep this loop's body a readable, bounded size. Returns the
        //    nonce used, so the caller can reuse it against the hostile
        //    worker too.
        let (nonce, received) = fuzz_ntp_iteration(&mut honest_time, &noise, &mut rng);

        // 7b. A document's syntax through the honest worker.
        fuzz_text_iteration(&mut honest_text, &noise, &mut rng);

        // 8. The hostile worker: framed noise replies into every client
        //    decoder. Each request crashes and replaces the worker, so
        //    every iteration sees fresh noise.
        let _ = container_summary(&mut hostile, &rxe);
        let _ = manifest_summary(&mut hostile, &noise[..rng.at_most(noise.len())]);
        let _ = disassemble(&mut hostile, isa, 0, 0, 8, b"\x90\x90");
        let _ = render_help(&mut hostile, mode, Styling::Colour, "en-US", HELP_TEMPLATE);
        let _ = rasterise_icon(&mut hostile, side, SVG_TEMPLATE, &mut NoFonts);
        if let Ok(fitted) = render_thumbnail(&mut hostile, side, None) {
            assert_fitted(&fitted, side);
        }
        let _ = drawn(&mut hostile, wallpaper_w, wallpaper_h, fit, &png_template());
        hostile_document_iteration(&mut hostile);
        let hostile_txn = tairix_net::ntp::Transaction {
            server: 0,
            nonce: tairix_net::ntp::NtpTimestamp::from_raw(nonce),
            sent_at: tairix_abi::time::Duration64::ZERO,
        };
        let _ = evaluate_datagram(
            &mut hostile,
            &hostile_txn,
            received,
            &ntp_reply_template(nonce),
        );
        fuzz_text_iteration(&mut hostile, &noise, &mut rng);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// An honest image worker whose replies are now and then corrupted. Framed
/// noise is turned away at its first tag check; this is what reaches the
/// validation behind it — descriptions, bands and kept bytes that are
/// nearly right.
struct MutatingWorker {
    honest: ImageRenderService,
    rng: Prng,
}

impl Service for MutatingWorker {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let mut reply = self.honest.handle(request);
        if self.rng.below(3) == 0 && !reply.is_empty() {
            match self.rng.below(4) {
                0 => {
                    let at = self.rng.below(reply.len());
                    reply[at] ^= 1 << self.rng.below(8);
                }
                1 => {
                    let at = self.rng.below(reply.len());
                    reply[at] = self.rng.next_u8();
                }
                2 => {
                    let keep = self.rng.at_most(reply.len());
                    reply.truncate(keep);
                }
                _ => {
                    for _ in 0..=self.rng.below(8) {
                        reply.push(self.rng.next_u8());
                    }
                }
            }
        }
        reply
    }
}

#[test]
fn nearly_right_edit_replies_are_refused_or_hold_their_description() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "nearly_right_edit_replies_are_refused_or_hold_their_description",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut seeds = Prng::new(rng.next_u64());
    let mut sandbox = ParserSandbox::new(
        LoopbackLauncher::new(move || MutatingWorker {
            honest: ImageRenderService::default(),
            rng: Prng::new(seeds.next_u64()),
        }),
        SilentSink,
    );
    let templates = edit_templates();
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let document = rng.pick(&templates);
            if send_document(&mut sandbox, document).is_err() {
                continue;
            }
            if let Ok(opened) = open_edit(&mut sandbox, *rng.pick(&EDIT_FORMATS)) {
                read_every_entry(&mut sandbox, opened);
            }
            let _ = close_edit(&mut sandbox);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

/// The key the audio worker's cache is built over; the harness draws none.
const AUDIO_SEED: tairix_hash::HashSeed = tairix_hash::HashSeed::from_words(7, 11);

/// Frames per block an audio decode asks for, chosen per iteration.
const AUDIO_BLOCKS: [u32; 4] = [1, 7, 512, tairix_sandbox::audiodecode::MAX_BLOCK_FRAMES];

/// What an audio request came to, owned.
#[derive(Debug, PartialEq)]
enum AudioOutcome {
    Opened,
    Block(u64, Vec<u8>),
    Ended(u64),
    Sought(u64),
    Refused(tairix_sandbox::audiodecode::AudioRefusal),
}

/// Records the frames an audio worker answers with.
struct Answers(Vec<Vec<u8>>);

impl FrameOut for Answers {
    fn frame(&mut self, payload: &[u8]) -> Result<(), tairix_sandbox::proto::ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

/// Carry the client's waiting request through `worker` to its answer,
/// supplying every need from `file`. An honest worker is always believed,
/// and answers every frame with exactly one.
fn audio_ask(
    worker: &mut tairix_sandbox::audiodecode::AudioDecodeService,
    client: &mut tairix_sandbox::audiodecode::AudioDecodeClient,
    file: &[u8],
    answers: &mut Vec<Vec<u8>>,
) -> AudioOutcome {
    use tairix_sandbox::audiodecode::{DecodeEvent, CACHE_PAGES};
    for _ in 0..=CACHE_PAGES {
        let request = client.outgoing().expect("a request waits").to_vec();
        client.sent();
        let mut out = Answers(Vec::new());
        assert_eq!(worker.handle(&request, &mut out), SessionStep::Continue);
        assert_eq!(out.0.len(), 1, "one answer a frame");
        let answer = out.0.remove(0);
        let event = client
            .on_frame(&answer)
            .expect("an honest worker is believed");
        let outcome = match event {
            DecodeEvent::Need { offset, len } => {
                let start = usize::try_from(offset).expect("small");
                client
                    .supply(&file[start..start + len])
                    .expect("the asked length");
                None
            }
            DecodeEvent::Opened => Some(AudioOutcome::Opened),
            DecodeEvent::Block { position, pcm } => {
                Some(AudioOutcome::Block(position, pcm.to_vec()))
            }
            DecodeEvent::Ended { position } => Some(AudioOutcome::Ended(position)),
            DecodeEvent::Sought { position } => Some(AudioOutcome::Sought(position)),
            DecodeEvent::Refused(refusal) => Some(AudioOutcome::Refused(refusal)),
            other => panic!("{other:?} without a replacement"),
        };
        answers.push(answer);
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
    panic!("a request needed more exchanges than the cache holds pages");
}

/// A WAVE file of `tag`-coded samples, `bits` wide, over `data`.
fn audio_wave(tag: u16, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
    let align = channels * bits.div_ceil(8);
    let mut format = Vec::new();
    for field in [tag, channels] {
        format.extend(field.to_le_bytes());
    }
    format.extend(8000u32.to_le_bytes());
    format.extend((8000 * u32::from(align)).to_le_bytes());
    format.extend(align.to_le_bytes());
    format.extend(bits.to_le_bytes());
    let mut info = b"INFO".to_vec();
    info.extend(b"INAM\x05\0\0\0title\0");
    let body = [
        b"fmt \x10\0\0\0".to_vec(),
        format,
        b"LIST".to_vec(),
        u32::try_from(info.len())
            .expect("small")
            .to_le_bytes()
            .to_vec(),
        info,
        b"data".to_vec(),
        u32::try_from(data.len())
            .expect("small")
            .to_le_bytes()
            .to_vec(),
        data.to_vec(),
    ]
    .concat();
    [
        b"RIFF".to_vec(),
        u32::try_from(body.len() + 4)
            .expect("small")
            .to_le_bytes()
            .to_vec(),
        b"WAVE".to_vec(),
        body,
    ]
    .concat()
}

/// An AU file of `encoding` over `data`, with an annotation.
fn audio_au(encoding: u32, channels: u32, data: &[u8]) -> Vec<u8> {
    let mut file = b".snd".to_vec();
    for field in [
        32,
        u32::try_from(data.len()).expect("small"),
        encoding,
        8000,
        channels,
    ] {
        file.extend(field.to_be_bytes());
    }
    file.extend(b"annotate");
    file.extend(data);
    file
}

fn audio_templates(rng: &mut Prng) -> Vec<Vec<u8>> {
    let mut data = vec![0u8; 2400];
    rng.fill(&mut data);
    vec![
        audio_wave(1, 2, 16, &data),
        audio_wave(1, 1, 8, &data[..999]),
        audio_wave(3, 1, 32, &data),
        audio_wave(7, 2, 8, &data),
        audio_au(3, 2, &data),
        audio_au(23, 1, &data),
        audio_au(27, 1, &data[..777]),
    ]
}

/// What an in-process decode of `file` in blocks of `frames` comes to:
/// the open's outcome, then each block's.
fn audio_direct(file: &[u8], frames: u32) -> Vec<AudioOutcome> {
    use tairix_sandbox::audiodecode::{AudioRefusal, LIMITS};
    let mut input = file;
    let mut source = match tairix_sound::PcmSource::open(&mut input, &LIMITS) {
        Ok(source) => source,
        Err(err) => return vec![AudioOutcome::Refused(AudioRefusal::Decode(err))],
    };
    let frame_bytes = source.info().frame_bytes();
    let mut block = vec![0u8; frames as usize * frame_bytes];
    let mut outcomes = vec![AudioOutcome::Opened];
    loop {
        let position = source.position();
        match source.next_block(&mut input, &mut block) {
            Ok(0) => {
                outcomes.push(AudioOutcome::Ended(position));
                return outcomes;
            }
            Ok(written) => {
                outcomes.push(AudioOutcome::Block(
                    position,
                    block[..written * frame_bytes].to_vec(),
                ));
            }
            Err(err) => {
                outcomes.push(AudioOutcome::Refused(AudioRefusal::Decode(err)));
                return outcomes;
            }
        }
    }
}

/// One audio iteration: a mutated or truncated file — or noise — decoded
/// through an honest worker exactly as in process, a seek checked against
/// the in-process stream, then nearly right answers and noise fed to the
/// client in every state, and noise fed to the worker as requests.
fn fuzz_audio_iteration(noise: &[u8], rng: &mut Prng) {
    use tairix_sandbox::audiodecode::{AudioDecodeClient, AudioDecodeService};
    let mut templates = audio_templates(rng);
    let pick = rng.below(templates.len() + 1);
    let mut file = if pick == templates.len() {
        noise.to_vec()
    } else {
        templates.swap_remove(pick)
    };
    for _ in 0..rng.at_most(4) {
        if !file.is_empty() {
            let at = rng.below(file.len());
            file[at] ^= rng.next_u8();
        }
    }
    file.truncate(file.len() - rng.at_most(file.len() / 4));

    let frames = *rng.pick(&AUDIO_BLOCKS);
    let expected = audio_direct(&file, frames);
    let mut worker = AudioDecodeService::new();
    let mut client = AudioDecodeClient::new();
    let mut answers = Vec::new();
    client
        .open(file.len() as u64, None, AUDIO_SEED)
        .expect("idle");
    let mut got = vec![audio_ask(&mut worker, &mut client, &file, &mut answers)];
    while matches!(
        got.last(),
        Some(AudioOutcome::Opened | AudioOutcome::Block(..))
    ) {
        client.decode(frames).expect("idle");
        got.push(audio_ask(&mut worker, &mut client, &file, &mut answers));
    }
    assert_eq!(
        got, expected,
        "the sandbox decoded otherwise than the process"
    );

    if let Some(info) = client.info().copied() {
        let target = info
            .frames
            .map_or(0, |stated| rng.next_u64() % (stated + 2));
        client.seek(target).expect("idle");
        let sought = audio_ask(&mut worker, &mut client, &file, &mut answers);
        if info.seekable && info.frames.is_some_and(|stated| target <= stated) {
            assert_eq!(sought, AudioOutcome::Sought(target));
        } else {
            assert!(matches!(sought, AudioOutcome::Refused(_)), "{sought:?}");
        }
    }

    for answer in answers.iter().take(8) {
        let mut nearly = answer.clone();
        let at = rng.below(nearly.len());
        nearly[at] ^= rng.next_u8() | 1;
        for mut client in hostile_audio_clients(&file) {
            let _ = client.on_frame(&nearly);
            let _ = client.on_frame(noise);
        }
    }

    let mut worker = AudioDecodeService::new();
    let mut out = Answers(Vec::new());
    let (mut offset, mut requests) = (0, 0);
    while offset < noise.len() {
        let len = rng.at_most(noise.len() - offset).max(1);
        assert_eq!(
            worker.handle(&noise[offset..offset + len], &mut out),
            SessionStep::Continue
        );
        offset += len;
        requests += 1;
    }
    assert_eq!(
        out.0.len(),
        requests,
        "one answer a frame, whatever it held"
    );
}

/// Clients waiting in each state an answer can arrive in: an open, a
/// decode, a seek.
fn hostile_audio_clients(file: &[u8]) -> Vec<tairix_sandbox::audiodecode::AudioDecodeClient> {
    use tairix_sandbox::audiodecode::{AudioDecodeClient, AudioDecodeService};
    let mut opening = AudioDecodeClient::new();
    opening
        .open(file.len() as u64, None, AUDIO_SEED)
        .expect("idle");
    opening.sent();
    let mut clients = vec![opening];
    let mut worker = AudioDecodeService::new();
    let mut opened = AudioDecodeClient::new();
    opened
        .open(file.len() as u64, None, AUDIO_SEED)
        .expect("idle");
    if audio_ask(&mut worker, &mut opened, file, &mut Vec::new()) == AudioOutcome::Opened {
        let mut decoding = opened;
        decoding.decode(4).expect("idle");
        decoding.sent();
        clients.push(decoding);
        let mut seeking = AudioDecodeClient::new();
        seeking
            .open(file.len() as u64, None, AUDIO_SEED)
            .expect("idle");
        let mut worker = AudioDecodeService::new();
        if audio_ask(&mut worker, &mut seeking, file, &mut Vec::new()) == AudioOutcome::Opened {
            seeking.seek(1).expect("idle");
            seeking.sent();
            clients.push(seeking);
        }
    }
    clients
}

#[test]
fn audio_decode_crosses_as_an_in_process_decode_and_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "audio_decode_crosses_as_an_in_process_decode_and_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        fuzz_audio_iteration(&noise, &mut rng);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS / 4 {
            break;
        }
    }
}
