//! How long disc transfers take against DFS 1.20's, the same steps timed by
//! BASIC's TIME: a load to within two centiseconds, and a save to within
//! the revolution longer DFS 1.20 takes over one.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::scenario::{run, Roms, Scenario};

/// Two loads and a save of a twenty-kilobyte file, each timed.
const TIMED: &str = r#"type:*SAVE BIG 1900 +5000\n;wait:8;type:CLS:T%=TIME:OSCLI"LOAD BIG":P.TIME-T%\n;wait:8;type:T%=TIME:OSCLI"LOAD BIG":P.TIME-T%\n;wait:8;type:T%=TIME:OSCLI"SAVE BIG2 1900 +5000":P.TIME-T%\n;wait:10"#;
/// DFS 1.20's centiseconds for each, and how far this DFS may be from them.
const DFS_1_20: [(u32, u32); 3] = [(351, 2), (347, 2), (411, 25)];

#[test]
fn loads_and_saves_take_as_long_as_dfs_1_20s() {
    let [os, basic, dfs] = [
        crate::rom::mos::model_b,
        crate::rom::basic::build,
        super::build,
    ]
    .map(|build| build().expect("each ROM assembles").bytes);
    let scenario = Scenario {
        name: String::from("timed"),
        discs: vec![String::from("blank80")],
        protected: false,
        checked: Vec::new(),
        ours: false,
        script: String::from(TIMED),
    };
    let rows = run(
        Roms {
            os: &os,
            basic: &basic,
            dfs: &dfs,
        },
        &scenario,
    )
    .expect("the scenario runs")
    .rows;
    let times: Vec<u32> = rows
        .iter()
        .filter_map(|row| row.trim().parse().ok())
        .collect();
    assert_eq!(times.len(), DFS_1_20.len(), "{rows:?}");
    for (time, (theirs, within)) in times.into_iter().zip(DFS_1_20) {
        assert!(
            time.abs_diff(theirs) <= within,
            "{time} against DFS 1.20's {theirs}"
        );
    }
}
