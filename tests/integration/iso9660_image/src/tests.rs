use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{FilesystemAttrs, FilesystemRead, NodeId};
use tairix_abi::DriverError;
use tairix_drv_fs_iso9660::Iso9660;

use super::*;

struct Image(Vec<u8>);

impl Block for Image {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: BLOCK as u32,
            block_count: (self.0.len() / BLOCK) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = lba as usize * BLOCK;
        buf.copy_from_slice(
            self.0
                .get(at..at + buf.len())
                .ok_or(DriverError::OutOfRange)?,
        );
        Ok(())
    }

    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn contents(fs: &mut Iso9660<Image>, node: NodeId) -> Vec<u8> {
    let mut out = vec![0u8; 4096];
    let len = fs.read_at(node, 0, &mut out).expect("reads");
    out.truncate(len);
    out
}

fn attr(fs: &mut Iso9660<Image>, node: NodeId, key: &[u8]) -> Option<Vec<u8>> {
    let mut value = [0u8; 64];
    fs.get_attr(node, key, &mut value)
        .expect("ok")
        .map(|len| value[..len].to_vec())
}

#[test]
fn the_riscos_discs_read_back_their_names_bytes_and_filetypes() {
    for (image, files) in [
        (riscos_disc(), &RISCOS_FILES[..]),
        (second_disc(), &SECOND_FILES[..]),
    ] {
        let mut fs = Iso9660::open(Image(image)).expect("mounts");
        let root = fs.root();
        for file in files {
            let node = fs
                .lookup(root, file.name.as_bytes())
                .expect("named as RISC OS reads it");
            assert_eq!(contents(&mut fs, node), file.content);
            let filetype = alloc::format!("{:03x}", file.filetype);
            assert_eq!(
                attr(&mut fs, node, b"acorn.filetype"),
                Some(filetype.into_bytes()),
                "{}",
                file.name
            );
        }
    }
}

#[test]
fn a_joliet_volume_identifier_is_padded_with_ucs2_spaces() {
    let disc = Disc {
        volume_id: b"PRIMARY".to_vec(),
        joliet: Some(Node::dir(&[], Vec::new())),
        ..Disc::default()
    };
    let image = master(&disc).image;
    let identifier = &image[17 * BLOCK + 40..17 * BLOCK + 72];
    assert_eq!(&identifier[..12], &ucs2("JOLIET")[..]);
    assert!(identifier[12..].chunks(2).all(|unit| unit == [0, b' ']));
    let primary = &image[16 * BLOCK + 40..16 * BLOCK + 72];
    assert_eq!(&primary[..7], b"PRIMARY");
    assert!(primary[7..].iter().all(|&byte| byte == b' '));
}
