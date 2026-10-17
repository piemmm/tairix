//! A RISC OS sprite area's control blocks, as the structure report lists
//! them.

use super::{read_area, read_header, word, MODE_AT, NAME_AT, NEXT_AT, SPRITE_HEADER_LEN};
use crate::inspect::{clean, Builder};

/// Bytes a sprite's name field holds.
const NAME_LEN: usize = 12;

/// Report `bytes`, a sprite area.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let Ok(area) = read_area(bytes) else {
        out.stop(0, "no sprite area: its header does not hold together");
        return;
    };
    out.heading(format_args!("RISC OS sprite area, {} bytes", bytes.len()));
    out.record(
        0,
        "area",
        12,
        format_args!(
            "{} sprites, the first at {:#x}, free space at {:#x}",
            area.count, area.first, area.end
        ),
    );
    let mut at = area.first;
    for index in 0..area.count {
        if out.full() || at >= area.end {
            return;
        }
        let place = usize::try_from(at).unwrap_or(usize::MAX);
        let Ok(length) = word(bytes, at.saturating_add(NEXT_AT)) else {
            out.stop(place, "a control block is cut short");
            return;
        };
        if length < SPRITE_HEADER_LEN {
            out.stop(place, "a sprite shorter than its own control block");
            return;
        }
        let name = usize::try_from(at.saturating_add(NAME_AT))
            .ok()
            .and_then(|start| bytes.get(start..start + NAME_LEN))
            .map(|name| name.split(|byte| *byte == 0).next().unwrap_or_default())
            .unwrap_or_default();
        let mode = word(bytes, at.saturating_add(MODE_AT)).unwrap_or_default();
        let detail = match read_header(bytes, at) {
            Ok(header) => alloc::format!(
                "sprite {}: \u{201c}{}\u{201d}, {} \u{d7} {}, mode word {mode:#010x}{}{}",
                index + 1,
                clean(name),
                header.width,
                header.height,
                if header.mask_at.is_some() { ", masked" } else { "" },
                if header.palette_entries > 0 {
                    alloc::format!(", {} palette entries", header.palette_entries)
                } else {
                    alloc::string::String::new()
                }
            ),
            Err(_) => alloc::format!(
                "sprite {}: \u{201c}{}\u{201d}, mode word {mode:#010x}, a control block that does not hold together",
                index + 1,
                clean(name)
            ),
        };
        out.record(
            place,
            "sprite",
            usize::try_from(length).unwrap_or(usize::MAX),
            format_args!("{detail}"),
        );
        let Some(next) = at.checked_add(length) else {
            return;
        };
        at = next;
    }
}
