//! Discovery of the on-disk font store: turning the family directories under
//! `/System/Fonts` into the resolved families [`crate::FontService`] serves
//! from.
//!
//! Discovery only ever *lists* the store and reads each family's small
//! `FontFamily` manifest text — it never reads a face's bytes. A face is
//! read once, lazily, the first time a request actually needs it
//! ([`FaceLoad`]), so a session that never draws a script never pays for the
//! face that covers it. [`FontStore`] is the seam that makes the scan itself
//! host-testable: a host test drives it from an in-memory fixture, the `Run`
//! binary drives it from the real filesystem through `tairix-rt`.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::font_ipc::{FamilyKey, FONT_MAX_FAMILIES};
use tairix_abi::Errno;
use tairix_fontface::FamilyManifest;
use tairix_log::{log, Event, Level, Sink};

use crate::events::FAMILY_SKIPPED;
use crate::service::{FaceCache, FamilyRuntime, FontService};
use crate::GlyphCache;

/// Supplies one face's raw bytes on first use.
///
/// A face's bytes are read once, on first use, and retained for the
/// service's life; a session that never touches a family's script never pays
/// for its face. Before that, what the face covers is read from its table
/// directory and `cmap` alone, through [`read_at`](Self::read_at). A host test
/// hands back bytes it already holds ([`Preloaded`]); the freestanding service
/// reads the handle it opened at discovery.
pub trait FaceLoad<'a> {
    /// Obtain the face's bytes, reading them if this is the first call.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the underlying read raises.
    fn load(&mut self) -> Result<&'a [u8], Errno>;

    /// The face's length in bytes.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the underlying query raises.
    fn size(&mut self) -> Result<u64, Errno>;

    /// Fill `buf` from `offset` into the face without reading the rest of
    /// it, answering how many bytes were filled: fewer only where the face
    /// ends.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the underlying read raises.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;
}

/// Fill `buf` from `offset` into `bytes`, answering how many bytes were
/// filled: fewer only where `bytes` ends.
fn read_held(bytes: &[u8], offset: u64, buf: &mut [u8]) -> usize {
    let start = usize::try_from(offset).map_or(bytes.len(), |at| at.min(bytes.len()));
    let filled = buf.len().min(bytes.len() - start);
    buf[..filled].copy_from_slice(&bytes[start..start + filled]);
    filled
}

/// A [`FaceLoad`] over bytes the caller already holds: the host-test loader.
pub struct Preloaded<'a>(pub &'a [u8]);

impl<'a> FaceLoad<'a> for Preloaded<'a> {
    fn load(&mut self) -> Result<&'a [u8], Errno> {
        Ok(self.0)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        u64::try_from(self.0.len()).map_err(|_| Errno::LengthOutOfRange)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        Ok(read_held(self.0, offset, buf))
    }
}

/// An open file in the font store.
pub trait StoreFile {
    /// The file's length in bytes.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the underlying query raises.
    fn size(&self) -> Result<u64, Errno>;

    /// Read into `buf` from `offset`, answering how many bytes were read:
    /// none only where the file ends.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the underlying read raises.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;
}

/// Fill `buf` from `offset` into `file`, answering how many bytes were
/// filled: fewer only where the file ends.
///
/// # Errors
///
/// Any [`Errno`] a read raises.
pub fn fill_at(file: &impl StoreFile, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
    let mut done = 0usize;
    while done < buf.len() {
        let at = u64::try_from(done)
            .ok()
            .and_then(|done| offset.checked_add(done))
            .ok_or(Errno::LengthOutOfRange)?;
        let read = file.read_at(at, &mut buf[done..])?;
        if read == 0 {
            break;
        }
        done += read;
    }
    Ok(done)
}

/// The whole of `file`.
///
/// The buffer is reserved fallibly, so a file larger than the heap can hold
/// refuses the read rather than aborting the service; the family is then
/// skipped or the glyph refused, and the rest of the store still serves.
///
/// # Errors
///
/// Any [`Errno`] the size query or a read raises, and
/// [`Errno::OutOfMemory`] when the buffer cannot be had.
pub fn read_whole(file: &impl StoreFile) -> Result<Vec<u8>, Errno> {
    let size = usize::try_from(file.size()?).map_err(|_| Errno::LengthOutOfRange)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| Errno::OutOfMemory)?;
    bytes.resize(size, 0);
    let done = fill_at(file, 0, &mut bytes)?;
    bytes.truncate(done);
    Ok(bytes)
}

/// A face read through the handle discovery opened: positioned reads until it
/// is read whole, once, on first use — never re-opened, never read early.
///
/// The bytes are leaked and retained for the service's life. The leak is
/// bounded by the store's own size (each declared face read at most once), not
/// by anything a caller can grow.
pub struct HandleFace<F> {
    /// The open handle, until a whole read succeeds; `None` when it could not
    /// be opened, which every read then reports.
    file: Option<F>,
    /// The face's bytes, once read.
    bytes: Option<&'static [u8]>,
}

impl<F> HandleFace<F> {
    /// A face to be read through `file`, or one that could not be opened.
    pub const fn new(file: Option<F>) -> Self {
        Self { file, bytes: None }
    }

    /// The open handle.
    fn file(&self) -> Result<&F, Errno> {
        self.file.as_ref().ok_or(Errno::NotFound)
    }
}

impl<F: StoreFile> FaceLoad<'static> for HandleFace<F> {
    fn load(&mut self) -> Result<&'static [u8], Errno> {
        if let Some(bytes) = self.bytes {
            return Ok(bytes);
        }
        // The handle is kept until a read succeeds, so a failed one is tried
        // again rather than losing the face for the service's life.
        let bytes = read_whole(self.file()?)?;
        self.file = None;
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        self.bytes = Some(leaked);
        Ok(leaked)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        match self.bytes {
            Some(bytes) => u64::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange),
            None => self.file()?.size(),
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        match self.bytes {
            Some(bytes) => Ok(read_held(bytes, offset, buf)),
            None => fill_at(self.file()?, offset, buf),
        }
    }
}

/// Read access to the on-disk font store the service discovers families
/// from.
///
/// Abstracted so the discovery and resolution logic in this crate is
/// host-testable against an in-memory fixture; the `Run` binary's own
/// implementation reads through `tairix-rt`.
pub trait FontStore<'a> {
    /// The store's family directory names, in whatever order the
    /// implementation happens to list them — [`discover`] sorts them before
    /// use, so the scan is deterministic regardless of listing order.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the store itself could not even be opened with. This is
    /// the one failure [`discover`] treats as immediately fatal, since no
    /// family can possibly be found without it.
    fn family_dirs(&mut self) -> Result<Vec<String>, Errno>;

    /// The `FontFamily` manifest text of family directory `dir`, or `None`
    /// when the directory carries no readable manifest.
    fn read_manifest(&mut self, dir: &str) -> Option<String>;

    /// A lazy loader for face file `face` inside family directory `dir`.
    fn face_loader(&mut self, dir: &str, face: &str) -> Box<dyn FaceLoad<'a> + 'a>;
}

/// Record a non-fatal "this family was skipped" warning.
fn log_skip(sink: &dyn Sink, message: &str) {
    let _ = log(
        sink,
        &Event {
            level: Level::Warn,
            id: FAMILY_SKIPPED,
            message,
            fields: &[],
        },
    );
}

/// Discover every usable family in `store` and build the [`FontService`]
/// that serves them.
///
/// The scan is bounded to [`FONT_MAX_FAMILIES`] directories and sorted by
/// directory name first, so the discovered order — and hence the families
/// reply [`crate::FontService`] serves — is deterministic regardless of the
/// store's own listing order. A directory whose name is not a valid family
/// key, that carries no manifest, or whose manifest does not parse is
/// skipped with a logged warning; it is never fatal on its own.
///
/// # Errors
///
/// [`Errno::NotFound`] when the store itself could not be listed, or when
/// not a single family was usable — a store with no usable family cannot
/// serve text at all, so this is the one fatal startup error.
pub fn discover<'a>(
    store: &mut impl FontStore<'a>,
    cache: GlyphCache,
    sink: &dyn Sink,
) -> Result<FontService<'a>, Errno> {
    let mut dirs = store.family_dirs().map_err(|_| Errno::NotFound)?;
    dirs.sort();

    let mut families = Vec::new();
    for dir in dirs.into_iter().take(FONT_MAX_FAMILIES) {
        let Ok(key) = FamilyKey::new(&dir) else {
            log_skip(
                sink,
                "fontd: a /System/Fonts entry is not a valid family key",
            );
            continue;
        };
        let Some(text) = store.read_manifest(&dir) else {
            log_skip(
                sink,
                "fontd: a /System/Fonts family carries no readable manifest",
            );
            continue;
        };
        let Ok(manifest) = FamilyManifest::parse(key, &text) else {
            log_skip(sink, "fontd: a /System/Fonts family manifest is malformed");
            continue;
        };
        let faces = manifest
            .faces()
            .iter()
            .map(|name| FaceCache::new(store.face_loader(&dir, name)))
            .collect();
        families.push(FamilyRuntime::new(
            key,
            manifest.label().to_string(),
            manifest.selectable_kind(),
            manifest.generic(),
            faces,
            manifest.fallback(),
        ));
    }

    if families.is_empty() {
        return Err(Errno::NotFound);
    }
    Ok(FontService::from_families(families, cache))
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! An in-memory [`FontStore`] fixture shared by this module's own
    //! discovery tests and the [`crate::service`] resolution/rendering
    //! tests, so both are exercised against the same small, fast, and
    //! deterministic store rather than each inventing its own.

    use alloc::rc::Rc;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::Cell;

    use tairix_abi::Errno;

    use super::{read_held, Box, FaceLoad, FontStore, Preloaded};

    /// One family directory the fixture store holds: its manifest text and
    /// its named faces' bytes.
    pub(crate) struct MemoryFamily<'a> {
        pub(crate) manifest: &'a str,
        pub(crate) faces: Vec<(&'a str, &'a [u8])>,
    }

    /// An in-memory store: directory name to [`MemoryFamily`].
    pub(crate) struct MemoryStore<'a> {
        pub(crate) dirs: Vec<(&'a str, MemoryFamily<'a>)>,
    }

    impl<'a> FontStore<'a> for MemoryStore<'a> {
        fn family_dirs(&mut self) -> Result<Vec<String>, Errno> {
            Ok(self
                .dirs
                .iter()
                .map(|&(name, _)| name.to_string())
                .collect())
        }

        fn read_manifest(&mut self, dir: &str) -> Option<String> {
            self.dirs
                .iter()
                .find(|&&(name, _)| name == dir)
                .map(|(_, family)| family.manifest.to_string())
        }

        fn face_loader(&mut self, dir: &str, face: &str) -> Box<dyn FaceLoad<'a> + 'a> {
            Box::new(Preloaded(self.face_bytes(dir, face)))
        }
    }

    impl<'a> MemoryStore<'a> {
        /// The bytes of face file `face` in family directory `dir`, or none.
        fn face_bytes(&self, dir: &str, face: &str) -> &'a [u8] {
            self.dirs
                .iter()
                .find(|&&(name, _)| name == dir)
                .and_then(|(_, family)| {
                    family
                        .faces
                        .iter()
                        .find(|&&(name, _)| name == face)
                        .map(|&(_, bytes)| bytes)
                })
                .unwrap_or(&[])
        }
    }

    /// What a [`Tallied`] face has been asked for.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct Tally {
        /// Whole reads.
        pub(crate) loads: usize,
        /// Bytes read by position.
        pub(crate) read: usize,
        /// Positioned reads still to refuse, as a read that fails would.
        pub(crate) refusing: usize,
    }

    /// A face over held bytes that tallies what it is asked for.
    struct Tallied<'a> {
        bytes: &'a [u8],
        tally: Rc<Cell<Tally>>,
    }

    impl<'a> FaceLoad<'a> for Tallied<'a> {
        fn load(&mut self) -> Result<&'a [u8], Errno> {
            let mut tally = self.tally.get();
            tally.loads += 1;
            self.tally.set(tally);
            Ok(self.bytes)
        }

        fn size(&mut self) -> Result<u64, Errno> {
            u64::try_from(self.bytes.len()).map_err(|_| Errno::LengthOutOfRange)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            let mut tally = self.tally.get();
            if tally.refusing > 0 {
                tally.refusing -= 1;
                self.tally.set(tally);
                return Err(Errno::MediumError);
            }
            let filled = read_held(self.bytes, offset, buf);
            tally.read += filled;
            self.tally.set(tally);
            Ok(filled)
        }
    }

    /// A [`MemoryStore`] whose faces tally what they are asked for, by face
    /// file name.
    pub(crate) struct TalliedStore<'a> {
        pub(crate) store: MemoryStore<'a>,
        pub(crate) tallies: Vec<(String, Rc<Cell<Tally>>)>,
    }

    impl TalliedStore<'_> {
        /// What the face file `face` has been asked for.
        pub(crate) fn tally(&self, face: &str) -> Tally {
            self.tallies
                .iter()
                .find(|(name, _)| name == face)
                .map(|(_, tally)| tally.get())
                .unwrap_or_default()
        }

        /// Refuse the face file `face`'s next `reads` positioned reads.
        pub(crate) fn refuse(&self, face: &str, reads: usize) {
            if let Some((_, tally)) = self.tallies.iter().find(|(name, _)| name == face) {
                let mut held = tally.get();
                held.refusing = reads;
                tally.set(held);
            }
        }
    }

    impl<'a> FontStore<'a> for TalliedStore<'a> {
        fn family_dirs(&mut self) -> Result<Vec<String>, Errno> {
            self.store.family_dirs()
        }

        fn read_manifest(&mut self, dir: &str) -> Option<String> {
            self.store.read_manifest(dir)
        }

        fn face_loader(&mut self, dir: &str, face: &str) -> Box<dyn FaceLoad<'a> + 'a> {
            let bytes = self.store.face_bytes(dir, face);
            let tally = Rc::new(Cell::new(Tally::default()));
            self.tallies.push((face.to_string(), Rc::clone(&tally)));
            Box::new(Tallied { bytes, tally })
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_abi::font_ipc::FamilyKind;
    use tairix_log::DiscardSink;
    use tairix_reclaim::{PressureBand, ReportedPressure};

    use super::fixtures::{MemoryFamily, MemoryStore};
    use super::{discover, fill_at, read_whole, FaceLoad, HandleFace, StoreFile};
    use crate::service::glyph_cache;

    /// A machine with plenty of RAM, so a test that is not about the bound
    /// gets a cache that comfortably holds what it asks for.
    const ROOMY_MACHINE_BYTES: u64 = 64 << 30;

    /// A byte-budgeted cache big enough for any of this module's tests, over
    /// a gauge a host test has nowhere to send pressure changes to.
    fn roomy_cache() -> crate::GlyphCache {
        static SINK: DiscardSink = DiscardSink;
        static GAUGE: ReportedPressure = ReportedPressure::unknown();
        GAUGE.report(PressureBand::Normal);
        glyph_cache(
            ROOMY_MACHINE_BYTES,
            &GAUGE,
            &SINK,
            crate::service::TEST_HASH_KEY,
        )
    }

    /// The committed `mono` face, small enough to embed directly in a test.
    const MONO_FACE: &[u8] = include_bytes!("../../../../lib/font/assets/mono/Inconsolata-EX.ttf");

    #[test]
    fn a_store_with_no_manifest_anywhere_is_a_fatal_startup_error() {
        let mut store = MemoryStore { dirs: Vec::new() };
        assert!(discover(&mut store, roomy_cache(), &DiscardSink).is_err());
    }

    #[test]
    fn an_unreadable_or_malformed_family_is_skipped_not_fatal() {
        let mut store = MemoryStore {
            dirs: vec![
                (
                    "not a key!",
                    MemoryFamily {
                        manifest: "label = Bad\nkind = proportional\nface = A.ttf\n",
                        faces: Vec::new(),
                    },
                ),
                (
                    "broken",
                    MemoryFamily {
                        manifest: "kind = proportional\n", // no label, no face
                        faces: Vec::new(),
                    },
                ),
                (
                    "mono",
                    MemoryFamily {
                        manifest: "label = Mono\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
            ],
        };
        let service =
            discover(&mut store, roomy_cache(), &DiscardSink).expect("one usable family is enough");
        assert_eq!(service.family_count(), 1);
    }

    #[test]
    fn discovery_ignores_a_directorys_listing_order() {
        let mut a_first = MemoryStore {
            dirs: vec![
                (
                    "mono",
                    MemoryFamily {
                        manifest: "label = Mono\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
                (
                    "zzz",
                    MemoryFamily {
                        manifest: "label = Zzz\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
            ],
        };
        let mut z_first = MemoryStore {
            dirs: vec![
                (
                    "zzz",
                    MemoryFamily {
                        manifest: "label = Zzz\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
                (
                    "mono",
                    MemoryFamily {
                        manifest: "label = Mono\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
            ],
        };
        let one = discover(&mut a_first, roomy_cache(), &DiscardSink).expect("discovers");
        let two = discover(&mut z_first, roomy_cache(), &DiscardSink).expect("discovers");
        assert_eq!(one.family_labels(), two.family_labels());
    }

    #[test]
    fn a_fallback_role_family_is_discovered_but_never_selectable() {
        let mut store = MemoryStore {
            dirs: vec![
                (
                    "mono",
                    MemoryFamily {
                        manifest: "label = Mono\nkind = monospace\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
                (
                    "fallback-only",
                    MemoryFamily {
                        manifest:
                            "label = Fallback Set\nkind = fallback\nface = Inconsolata-EX.ttf\n",
                        faces: vec![("Inconsolata-EX.ttf", MONO_FACE)],
                    },
                ),
            ],
        };
        let mut service = discover(&mut store, roomy_cache(), &DiscardSink).expect("discovers");
        assert_eq!(service.family_count(), 2, "both directories are discovered");
        let mut reply = vec![0u8; tairix_abi::font_ipc::FONT_MAX_FAMILIES_REPLY];
        let n = service.families_reply(&mut reply).expect("encodes");
        let list = tairix_abi::font_ipc::decode_families_reply(&reply[..n]).expect("decodes");
        assert_eq!(
            list.entries().len(),
            1,
            "the fallback-only set is never offered"
        );
        assert_eq!(list.entries()[0].kind, FamilyKind::Monospace);
        // Inconsolata EX's `hhea` is 939 up and 198 down on a 1024-unit em.
        assert_eq!(list.entries()[0].line_box(), 1110);
    }

    /// A store file of `bytes` that hands out at most three bytes a read, and
    /// can be made to refuse every read, counting what it was asked.
    struct Trickle {
        bytes: &'static [u8],
        refusing: bool,
        reads: alloc::rc::Rc<core::cell::Cell<usize>>,
    }

    impl Trickle {
        fn new(bytes: &'static [u8], refusing: bool) -> Self {
            Self {
                bytes,
                refusing,
                reads: alloc::rc::Rc::default(),
            }
        }
    }

    impl StoreFile for Trickle {
        fn size(&self) -> Result<u64, tairix_abi::Errno> {
            Ok(u64::try_from(self.bytes.len()).expect("small"))
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, tairix_abi::Errno> {
            self.reads.set(self.reads.get() + 1);
            if self.refusing {
                return Err(tairix_abi::Errno::MediumError);
            }
            let start = usize::try_from(offset)
                .expect("small")
                .min(self.bytes.len());
            let filled = buf.len().min(3).min(self.bytes.len() - start);
            buf[..filled].copy_from_slice(&self.bytes[start..start + filled]);
            Ok(filled)
        }
    }

    #[test]
    fn a_positioned_read_is_filled_across_short_reads_and_stops_where_the_file_ends() {
        let file = Trickle::new(b"0123456789", false);
        let mut buf = [0u8; 8];
        assert_eq!(fill_at(&file, 1, &mut buf), Ok(8));
        assert_eq!(&buf, b"12345678");
        assert_eq!(fill_at(&file, 6, &mut buf), Ok(4));
        assert_eq!(&buf[..4], b"6789");
        assert_eq!(read_whole(&file).as_deref(), Ok(&b"0123456789"[..]));

        let mut face = HandleFace::new(Some(Trickle::new(b"0123456789", false)));
        assert_eq!(face.size(), Ok(10));
        let mut three = [0u8; 3];
        assert_eq!(face.read_at(2, &mut three), Ok(3));
        assert_eq!(&three, b"234");
        let mut unopened = HandleFace::<Trickle>::new(None);
        assert_eq!(unopened.load(), Err(tairix_abi::Errno::NotFound));
        assert_eq!(unopened.size(), Err(tairix_abi::Errno::NotFound));
    }

    /// A failed whole read keeps the handle, so the face is not lost until the
    /// service restarts.
    #[test]
    fn a_failed_whole_read_keeps_the_handle_for_the_next_try() {
        let file = Trickle::new(b"0123456789", true);
        let reads = alloc::rc::Rc::clone(&file.reads);
        let mut face = HandleFace::new(Some(file));
        assert_eq!(face.load(), Err(tairix_abi::Errno::MediumError));
        let first = reads.get();
        assert!(first > 0);
        assert_eq!(
            face.load(),
            Err(tairix_abi::Errno::MediumError),
            "the handle is still there to read"
        );
        assert!(reads.get() > first, "and is read again");
    }
}
