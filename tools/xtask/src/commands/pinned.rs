//! Files a command takes from outside the tree, each held to the SHA-256 it
//! was reviewed at, so a different file is refused rather than recorded.

use std::path::Path;

/// A file and the SHA-256 it must hash to, in lowercase hexadecimal.
pub struct Pinned<'a> {
    pub file: &'a str,
    pub sha256: &'a str,
}

/// The `SingleStepTests` suites' MIT licence, which each suite's `LICENSE`
/// must be before a record calls it that.
pub const SINGLE_STEP_LICENSE: Pinned<'static> = Pinned {
    file: "LICENSE",
    sha256: "9b9e332ee11019f131e945d40e20ab0c48b52921754dc03fe62cc85b579e6c61",
};

impl Pinned<'_> {
    /// The file's bytes in `dir`, once they hash to the pin; `command` begins
    /// each refusal.
    pub fn read(&self, command: &str, dir: &Path) -> Result<Vec<u8>, String> {
        let path = dir.join(self.file);
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("{command}: cannot read {}: {e}", path.display()))?;
        if !hashes_to(&bytes, self.sha256) {
            return Err(format!(
                "{command}: {} does not hash to its pinned SHA-256",
                path.display()
            ));
        }
        Ok(bytes)
    }

    /// As [`Pinned::read`], the file taken as UTF-8 text.
    pub fn read_text(&self, command: &str, dir: &Path) -> Result<String, String> {
        String::from_utf8(self.read(command, dir)?)
            .map_err(|_| format!("{command}: {} is not UTF-8", dir.join(self.file).display()))
    }
}

/// Whether `bytes` hash to `sha256`, lowercase hexadecimal.
pub fn hashes_to(bytes: &[u8], sha256: &str) -> bool {
    tairix_util::hex::encode(&tairix_crypto::sha256(bytes)) == sha256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_read_only_while_it_hashes_to_its_pin() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "xtask-pinned");
        let dir = scratch.path();
        std::fs::write(dir.join("asset"), b"abc").expect("a scratch file");
        let pinned = Pinned {
            file: "asset",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert_eq!(pinned.read("test", dir), Ok(b"abc".to_vec()));
        assert_eq!(pinned.read_text("test", dir), Ok("abc".to_string()));
        std::fs::write(dir.join("asset"), b"abd").expect("a scratch file");
        let refused = pinned.read("test", dir).expect_err("a changed file");
        assert!(
            refused.contains("does not hash to its pinned SHA-256"),
            "{refused}"
        );
        assert!(Pinned {
            file: "absent",
            ..pinned
        }
        .read("test", dir)
        .is_err());
    }
}
