use alloc::vec::Vec;

use tairix_mix::{mix32, unit};
use tairix_parallel::JobRunner;
use tairix_util::mathf;

use super::*;

/// A ranking hands its places out largest first, those alike in the order
/// they were added — as one sort of them all would — however its runs were
/// shared among cores, across run boundaries and with many sizes alike.
#[test]
fn a_ranking_takes_its_places_largest_first_as_one_sort_would() {
    for count in [0usize, 1, 7, RUN - 1, RUN, RUN + 1, 3 * RUN + 1234] {
        let sizes: Vec<f64> = (0..count)
            .map(|index| {
                // Rounded to tenths, so most sizes recur many times.
                let draw = unit(mix32(u32::try_from(index).expect("index") ^ 0x2545));
                mathf::round(300.0 * draw) / 10.0
            })
            .collect();
        let mut expected: Vec<(f32, u32)> = sizes
            .iter()
            .enumerate()
            .map(|(index, &size)| (single(size), u32::try_from(index).expect("index")))
            .collect();
        expected.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let expected: Vec<u32> = expected.into_iter().map(|(_, index)| index).collect();
        let runners: [&dyn JobRunner; 3] = [
            &tairix_parallel::SERIAL,
            &tairix_parallel::Reversed::new(3),
            &tairix_parallel::Threaded::new(4),
        ];
        for runner in runners {
            let mut ranking = Ranking::default();
            assert!(ranking.reserve(count));
            for (index, &size) in sizes.iter().enumerate() {
                ranking.add(size, u32::try_from(index).expect("index"));
            }
            let mut units = 0;
            while !ranking.ranked() {
                ranking.rank(runner).expect("held");
                units += 1;
            }
            assert!(units <= count.div_ceil(RUN), "{units} units for {count}");
            let mut taken = Vec::new();
            while let Some(index) = ranking.next() {
                taken.push(index);
            }
            assert!(ranking.exhausted());
            assert_eq!(taken, expected, "{count} places");
        }
    }
}
