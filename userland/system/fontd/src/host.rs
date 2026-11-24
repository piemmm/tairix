//! The service's store as a directory on the host: one subdirectory per
//! family, each with its `FontFamily` manifest, read through the service's
//! own discovery. Host tools and tests serve fonts from it, so nothing about
//! resolution, fallback or outlines is done differently from the service on
//! TAIRiX; only the reading of files is.

use std::boxed::Box;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use tairix_abi::Errno;

use crate::discovery::{FaceLoad, FontStore, HandleFace, StoreFile};

/// A store directory on the host.
pub struct DirectoryStore {
    root: PathBuf,
}

impl DirectoryStore {
    /// The store whose family directories are in `root`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl FontStore<'static> for DirectoryStore {
    fn family_dirs(&mut self) -> Result<Vec<String>, Errno> {
        let mut dirs = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|_| Errno::NotFound)? {
            let entry = entry.map_err(|_| Errno::NotFound)?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    dirs.push(String::from(name));
                }
            }
        }
        Ok(dirs)
    }

    fn read_manifest(&mut self, dir: &str) -> Option<String> {
        fs::read_to_string(self.root.join(dir).join("FontFamily")).ok()
    }

    fn face_loader(&mut self, dir: &str, face: &str) -> Box<dyn FaceLoad<'static> + 'static> {
        let file = fs::File::open(self.root.join(dir).join(face))
            .ok()
            .map(HostFile);
        Box::new(HandleFace::new(file))
    }
}

/// A face file in the store, read where the service asks.
struct HostFile(fs::File);

impl StoreFile for HostFile {
    fn size(&self) -> Result<u64, Errno> {
        self.0
            .metadata()
            .map(|meta| meta.len())
            .map_err(|_| Errno::DeviceFault)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut file = &self.0;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read(buf))
            .map_err(|_| Errno::DeviceFault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_face_is_read_when_first_asked_for_not_when_discovered() {
        let root = std::env::temp_dir().join(std::format!("fontd-host-{}", std::process::id()));
        fs::create_dir_all(root.join("family")).expect("scratch store");
        let face = root.join("family").join("face.ttf");
        fs::write(&face, b"as discovered").expect("a face");
        let mut loader = DirectoryStore::new(root.clone()).face_loader("family", "face.ttf");
        fs::write(&face, b"as first asked for").expect("rewritten in place");
        assert_eq!(loader.load().expect("read"), b"as first asked for");
        fs::remove_dir_all(&root).expect("clean up");
    }

    #[test]
    fn a_store_that_cannot_be_listed_is_refused() {
        let mut store = DirectoryStore::new(PathBuf::from("/no such store"));
        assert!(store.family_dirs().is_err());
    }
}
