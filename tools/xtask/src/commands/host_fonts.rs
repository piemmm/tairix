//! A build-time font provider over the committed store, so the icon
//! verification the image build runs sees the same glyphs the desktop will.
//!
//! An icon asset carrying `<text>` is refused by the SVG decoder unless a
//! provider can furnish its faces, and the build refuses an icon the desktop
//! could not draw. Verifying such an asset therefore needs the same
//! resolution the running system does — so this drives the **real** `fontd`
//! service over the committed `lib/font/assets/` tree through the store
//! seam that service already has for host testing, asked through `lib/font`'s
//! own outline request and paging. Nothing about family resolution,
//! per-scalar fallback, weight instancing or outline decoding is
//! re-implemented here.

use std::path::Path;

use tairix_abi::font_ipc::{FamilyKey, FontStretch, FontStyle, FontWeight, FONT_MAX_OUTLINE_REPLY};
use tairix_font::{
    ask_outlines, page_outlines, seam_metrics, seam_outlines, wire_face, OutlineReply, FACE_PROBE,
};
use tairix_fontd::discovery::discover;
use tairix_fontd::host::DirectoryStore;
use tairix_fontd::FontService;
use tairix_log::DiscardSink;
use tairix_reclaim::{PressureBand, ReportedPressure};
use tairix_svg::font::{
    FaceId, FaceMetrics, FaceRequest, FontProvider, FontUnavailable, GlyphOutline,
};

/// The RAM figure the build-time glyph cache is sized from: a build host has
/// memory to spare and the cache lives for one verification pass.
const HOST_RAM_BYTES: u64 = 1 << 30;

/// The hash key the build-time cache files its entries under. A build is not
/// serving hostile clients, so a fixed key keeps a verification pass
/// reproducible.
const HOST_HASH_KEY: tairix_hash::HashSeed =
    tairix_hash::HashSeed::from_words(0x7873_6B00_464F_4E54, 0x7873_6B00_464F_4E55);

/// The build's font provider: the real service, over the committed store.
pub struct HostFonts {
    service: FontService<'static>,
    faces: Vec<(FamilyKey, (FontWeight, FontStyle, FontStretch))>,
    reply: Vec<u8>,
}

impl HostFonts {
    /// Build a provider over the font assets under `assets_root` (the
    /// `lib/font/assets/` tree the image build plants as `/System/Fonts`).
    ///
    /// # Errors
    ///
    /// A message when the store cannot be listed or holds no usable family,
    /// which is the same fatal startup condition the running service has.
    pub fn open(assets_root: &Path) -> Result<Self, String> {
        static SINK: DiscardSink = DiscardSink;
        let gauge: &'static ReportedPressure = Box::leak(Box::new(ReportedPressure::unknown()));
        gauge.report(PressureBand::Normal);
        let cache = tairix_fontd::glyph_cache(HOST_RAM_BYTES, gauge, &SINK, HOST_HASH_KEY);
        let mut store = DirectoryStore::new(assets_root.to_path_buf());
        let service = discover(&mut store, cache, &SINK).map_err(|err| {
            format!(
                "font store at {} is unusable: {err:?}",
                assets_root.display()
            )
        })?;
        Ok(Self {
            service,
            faces: Vec::new(),
            reply: vec![0; FONT_MAX_OUTLINE_REPLY],
        })
    }

    /// Ask the service for the outlines of a prefix of `run` in `face`,
    /// exactly as a client would.
    fn ask(
        &mut self,
        run: &[char],
        face: (FamilyKey, (FontWeight, FontStyle, FontStretch)),
    ) -> Option<OutlineReply> {
        let (family, instance) = face;
        let service = &mut self.service;
        ask_outlines(run, family, instance, &mut self.reply, |request, reply| {
            Some(service.handle(request, reply))
        })
    }
}

impl FontProvider for HostFonts {
    fn select(&mut self, req: &FaceRequest<'_>) -> Result<FaceMetrics, FontUnavailable> {
        let face = wire_face(req)?;
        let held = self.faces.iter().position(|known| *known == face);
        let reply = self.ask(&[FACE_PROBE], face).ok_or(FontUnavailable)?;
        let index = if let Some(index) = held {
            index
        } else {
            self.faces.push(face);
            self.faces.len() - 1
        };
        let id = FaceId::new(u32::try_from(index).map_err(|_| FontUnavailable)?);
        seam_metrics(id, reply.face)
    }

    fn outlines(
        &mut self,
        face: FaceId,
        run: &[char],
        out: &mut Vec<GlyphOutline>,
    ) -> Result<(), FontUnavailable> {
        let wire = *self.faces.get(face.get() as usize).ok_or(FontUnavailable)?;
        page_outlines(
            self,
            run,
            |fonts, asked| fonts.ask(asked, wire),
            |_, _, reply| out.extend(seam_outlines(reply)),
        )
        .ok_or(FontUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tairix_abi::font_ipc::FONT_MAX_GLYPH_RUN;
    use tairix_svg::font::{FaceId, FaceRequest, FontProvider, FontStyle};

    use super::super::font_store::ASSETS_DIR;
    use super::HostFonts;

    fn committed_store() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(ASSETS_DIR)
    }

    /// The committed store's provider, and its sans-serif face.
    fn sans_serif() -> (HostFonts, FaceId) {
        let mut fonts = HostFonts::open(&committed_store()).expect("the committed store");
        let face = fonts
            .select(&FaceRequest {
                family: "sans-serif",
                weight: 400,
                style: FontStyle::Normal,
                stretch: 10_000,
            })
            .expect("a sans-serif face");
        (fonts, face.id)
    }

    #[test]
    fn the_committed_store_draws_text_through_the_service() {
        let (mut fonts, face) = sans_serif();
        let mut outlines = Vec::new();
        // The alef is drawn by a fallback face, read only once it is asked for.
        fonts
            .outlines(face, &['T', 'a', '\u{05D0}'], &mut outlines)
            .expect("outlines");
        assert_eq!(outlines.len(), 3);
        assert!(outlines.iter().all(|glyph| glyph.advance > 0.0));
    }

    #[test]
    fn a_run_longer_than_one_request_carries_is_drawn_whole() {
        let (mut fonts, face) = sans_serif();
        let run = ['a'; FONT_MAX_GLYPH_RUN + 8];
        let mut outlines = Vec::new();
        fonts.outlines(face, &run, &mut outlines).expect("outlines");
        assert_eq!(outlines.len(), run.len());
    }

    #[test]
    fn a_store_that_cannot_be_listed_is_refused() {
        assert!(HostFonts::open(&committed_store().join("no such store")).is_err());
    }
}
