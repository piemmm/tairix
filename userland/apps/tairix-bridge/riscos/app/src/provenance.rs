//! Which session a launch runs in (`plans/RISCOS-EMULATOR.md` decision 9,
//! §15.3): the shared one for a file from the user's own storage — the
//! documents folder's volume — and an isolated one of its own for anything
//! else, unless the user runs it in the shared session; isolating every
//! launch withdraws that choice.

/// A volume's identity, as a file's stat names it: all zeros for a backing
/// with none.
pub type Volume = [u8; 16];

/// What a launch asks of its session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asked {
    /// Where the file's provenance places it.
    ByProvenance,
    /// The shared session, by the user's own command.
    Shared,
}

/// Where a launch runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// In the shared session.
    Shared,
    /// In an isolated session of its own.
    Isolated,
}

/// Where a file on `volume` runs when asked `asked`, the documents folder on
/// `documents` where it was granted, every launch isolated where
/// `isolate_all`.
#[must_use]
pub fn placement(
    volume: Volume,
    documents: Option<Volume>,
    asked: Asked,
    isolate_all: bool,
) -> Placement {
    let known = volume != Volume::default();
    let own = known && documents == Some(volume);
    if isolate_all || !(own || asked == Asked::Shared) {
        Placement::Isolated
    } else {
        Placement::Shared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: Volume = [7; 16];
    const REMOVABLE: Volume = [9; 16];
    const UNKNOWN: Volume = [0; 16];

    #[test]
    fn a_file_on_the_documents_folder_s_volume_runs_in_the_shared_session() {
        let at = |volume| placement(volume, Some(OWN), Asked::ByProvenance, false);
        assert_eq!(at(OWN), Placement::Shared);
        assert_eq!(at(REMOVABLE), Placement::Isolated);
        assert_eq!(at(UNKNOWN), Placement::Isolated, "no known provenance");
    }

    #[test]
    fn with_no_documents_folder_nothing_is_the_user_s_own() {
        assert_eq!(
            placement(OWN, None, Asked::ByProvenance, false),
            Placement::Isolated
        );
        assert_eq!(
            placement(UNKNOWN, Some(UNKNOWN), Asked::ByProvenance, false),
            Placement::Isolated,
            "a folder of no identity vouches for nothing"
        );
    }

    #[test]
    fn the_user_may_run_anything_in_the_shared_session() {
        for volume in [OWN, REMOVABLE, UNKNOWN] {
            assert_eq!(
                placement(volume, Some(OWN), Asked::Shared, false),
                Placement::Shared
            );
        }
    }

    #[test]
    fn isolating_every_launch_tightens_and_withdraws_the_choice() {
        for asked in [Asked::ByProvenance, Asked::Shared] {
            for volume in [OWN, REMOVABLE, UNKNOWN] {
                assert_eq!(
                    placement(volume, Some(OWN), asked, true),
                    Placement::Isolated
                );
            }
        }
    }
}
