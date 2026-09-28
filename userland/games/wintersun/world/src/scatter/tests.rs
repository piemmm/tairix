use alloc::vec::Vec;

use super::{for_chunk, Footing, ScatterKind, Scattered, SCATTER_STEP};
use crate::biome::Biome;
use crate::blend::Blend;
use crate::geom::{CellCoord, CELL_SUB_UNITS};
use crate::seed::SeedKey;
use tairix_wintersun_net::value::ChunkCoord;

const KEY: SeedKey = SeedKey::new(0x5CA7_7E12);

fn open_ground(_: CellCoord) -> Footing {
    Footing {
        biomes: Blend::solid(Biome::BorealForest),
        slope: 0.1,
        submerged: false,
        cleared: false,
    }
}

fn barren(_: CellCoord) -> Footing {
    Footing {
        biomes: Blend::solid(Biome::OpenWater),
        slope: 0.1,
        submerged: true,
        cleared: false,
    }
}

#[test]
fn an_exclusion_radius_never_reaches_past_one_ring() {
    for kind in [
        ScatterKind::Tree,
        ScatterKind::Shrub,
        ScatterKind::Boulder,
        ScatterKind::Resource,
    ] {
        assert!(
            kind.exclusion_cells() <= SCATTER_STEP,
            "an exclusion wider than a scatter cell would need a wider halo"
        );
        assert!(kind.slope_limit() > 0.0);
    }
}

#[test]
fn nothing_stands_on_water_or_on_a_cleared_cell() {
    let wet = for_chunk(KEY, ChunkCoord { x: 0, y: 0 }, &barren).expect("fits");
    assert!(wet.is_empty());

    let paved = for_chunk(KEY, ChunkCoord { x: 0, y: 0 }, &|_| Footing {
        cleared: true,
        ..open_ground(CellCoord::new(0, 0))
    })
    .expect("fits");
    assert!(paved.is_empty());
}

#[test]
fn a_face_grows_nothing() {
    let cliff = for_chunk(KEY, ChunkCoord { x: 0, y: 0 }, &|_| Footing {
        slope: 99.0,
        ..open_ground(CellCoord::new(0, 0))
    })
    .expect("fits");
    assert!(cliff.is_empty());
}

#[test]
fn a_forest_is_placed_with_spacing_rather_than_in_clumps() {
    let items = for_chunk(KEY, ChunkCoord { x: 0, y: 0 }, &open_ground).expect("fits");
    assert!(!items.is_empty(), "boreal forest should grow something");
    assert_min_spacing(&items);
}

#[test]
fn adjacent_chunks_agree_along_their_seam() {
    // The halo rule is what this tests: two chunks generated independently
    // must not place two items on top of each other across the boundary.
    let mut all = Vec::new();
    for x in 0..2 {
        for y in 0..2 {
            all.extend(for_chunk(KEY, ChunkCoord { x, y }, &open_ground).expect("fits"));
        }
    }
    assert_min_spacing(&all);
}

#[test]
fn the_order_chunks_are_generated_in_changes_nothing() {
    let forward = for_chunk(KEY, ChunkCoord { x: 3, y: -2 }, &open_ground).expect("fits");
    let _ = for_chunk(KEY, ChunkCoord { x: -9, y: 40 }, &open_ground).expect("fits");
    let again = for_chunk(KEY, ChunkCoord { x: 3, y: -2 }, &open_ground).expect("fits");
    assert_eq!(forward, again);
}

#[test]
fn items_stay_inside_the_chunk_that_placed_them() {
    let chunk = ChunkCoord { x: 2, y: -3 };
    let origin = crate::geom::chunk_origin(chunk);
    let low = i64::from(origin.x) * i64::from(CELL_SUB_UNITS);
    let span = i64::from(crate::geom::CHUNK_CELLS) * i64::from(CELL_SUB_UNITS);
    for item in for_chunk(KEY, chunk, &open_ground).expect("fits") {
        let x = i64::from(item.at.x);
        assert!(
            x >= low - i64::from(CELL_SUB_UNITS) && x < low + span + i64::from(CELL_SUB_UNITS),
            "an item was placed outside its chunk"
        );
    }
}

#[test]
fn a_seed_change_replants_the_ground() {
    let a = for_chunk(KEY, ChunkCoord { x: 0, y: 0 }, &open_ground).expect("fits");
    let b = for_chunk(SeedKey::new(77), ChunkCoord { x: 0, y: 0 }, &open_ground).expect("fits");
    assert_ne!(a, b);
}

/// No two items are closer than the narrower of their exclusions.
///
/// One-directional by design: the wider-excluding item yields to the
/// narrower one, never both to each other.
fn assert_min_spacing(items: &[Scattered]) {
    let sub = i64::from(CELL_SUB_UNITS);
    for (index, a) in items.iter().enumerate() {
        for b in &items[index + 1..] {
            let dx = i64::from(a.at.x - b.at.x).abs();
            let dy = i64::from(a.at.y - b.at.y).abs();
            let apart = dx.max(dy);
            let closest = i64::from(a.kind.exclusion_cells().min(b.kind.exclusion_cells()));
            assert!(
                apart >= (closest - 1) * sub,
                "two items {apart} sub-units apart with a {closest}-cell exclusion"
            );
        }
    }
}

#[test]
fn each_biome_grows_only_what_belongs_in_it() {
    use crate::blend::Kind;
    for &biome in Biome::ALL {
        let items = for_chunk(KEY, ChunkCoord { x: 1, y: 1 }, &|_| Footing {
            biomes: Blend::solid(biome),
            ..open_ground(CellCoord::new(0, 0))
        })
        .expect("fits");
        for item in &items {
            assert_eq!(item.host, biome);
        }
        let trees = items.iter().any(|i| i.kind == ScatterKind::Tree);
        let wooded = matches!(
            biome,
            Biome::BorealForest
                | Biome::TemperateConiferForest
                | Biome::TemperateBroadleafForest
                | Biome::TemperateRainforest
                | Biome::MediterraneanWoodland
                | Biome::Savanna
                | Biome::TropicalDryForest
                | Biome::TropicalRainforest
                | Biome::Mangrove
                | Biome::SwampForest
        );
        assert!(!trees || wooded, "a tree grew in {biome:?}");
        if matches!(biome, Biome::OpenWater | Biome::IceSheet) {
            assert!(items.is_empty(), "something stood in {biome:?}");
        }
        if wooded && !matches!(biome, Biome::Savanna) {
            assert!(trees, "{biome:?} grew no trees");
        }
    }
}
