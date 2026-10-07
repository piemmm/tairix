use super::*;

/// How rank the grass stands at `at` against every pat near it, met one by
/// one: smothered beneath any of them, else the rankest any leaves it.
fn rank_among(at: (f64, f64), seed: u32) -> Option<f64> {
    about(at, 2.0 * CELL, seed).try_fold(0.0, |most: f64, pat| {
        pat.rank_at(at).map(|rank| most.max(rank))
    })
}

/// The grass reads only its own cell's pat, yet stands as every pat about it
/// would have it: smothered beneath a pat, rank about one, grazed short
/// between.
#[test]
fn the_grass_stands_rank_about_every_pat() {
    let (mut rank_seen, mut grazed, mut smothered) = (0u32, 0u32, 0u32);
    for index in 0..40_000u32 {
        let at = (
            90.0 * unit(mix32(index ^ 0x11)) - 45.0,
            90.0 * unit(mix32(index ^ 0x22)) - 45.0,
        );
        let (own, among) = (rank(at, 7), rank_among(at, 7));
        match (own, among) {
            (Some(own), Some(among)) => {
                assert!((own - among).abs() < 1e-12, "{at:?}: {own} against {among}");
            }
            (None, None) => {}
            _ => panic!("{at:?}: {own:?} against {among:?}"),
        }
        match own {
            None => smothered += 1,
            Some(own) if own > 0.3 => rank_seen += 1,
            Some(own) if own <= 0.0 => grazed += 1,
            Some(_) => {}
        }
    }
    assert!(
        rank_seen > 100 && grazed > 20_000 && smothered > 10,
        "{rank_seen} rank, {grazed} grazed, {smothered} smothered"
    );
}

/// A fresh pat lies on grass grazed as short as the rest; the grass about one
/// some weeks old stands rankest; as it crumbles stock graze it down again.
#[test]
fn the_grass_grows_rank_while_its_pat_lies() {
    let rim = |age: f64| {
        let pat = Pat {
            at: (0.0, 0.0),
            radius: 0.12,
            age,
            key: 0,
        };
        pat.rank_at((1.2 * pat.radius, 0.0))
            .expect("grass at its rim")
    };
    let (fresh, weeks, crumbling) = (rim(0.0), rim(0.45), rim(0.98));
    assert!(
        fresh < 1e-12 && weeks > 0.9 && crumbling < 0.5 * weeks,
        "{fresh}, {weeks}, {crumbling}"
    );
}

/// Pats lie about one to twenty square metres, more where the stock
/// gathered, each a hand or two across.
#[test]
fn pats_lie_as_thickly_as_stock_leave_them() {
    let pats: alloc::vec::Vec<Pat> = about((0.0, 0.0), 120.0, 3).collect();
    let area = core::f64::consts::PI * 120.0 * 120.0;
    let per = area / crate::vector::real(pats.len());
    assert!(per > 12.0 && per < 40.0, "a pat to {per} square metres");
    assert!(pats
        .iter()
        .all(|pat| (RADIUS.0..=RADIUS.1).contains(&pat.radius)));
    let fresh = pats.iter().filter(|pat| pat.age < 0.3).count();
    assert!(fresh * 4 < pats.len(), "{fresh} of {} fresh", pats.len());
}
