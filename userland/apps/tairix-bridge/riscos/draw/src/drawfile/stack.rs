//! How deep a Draw file's groups and tagged objects may nest before the
//! supervisor stack a walk of them runs on overflows, as RISC OS 5.30's
//! DrawFile overflows its own when called from BASIC: each level of a walk
//! takes its kind's share of the stack, so mixed nesting adds up as a stack
//! does, and no file nests deeper than the host's own stack can follow.

/// The stack each walk shares out, in units with no meaning but the shares.
const STACK: u32 = 1 << 20;

/// A level of nesting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Level {
    Group,
    Tagged,
}

/// A walk's share of the stack for each level, by its kind.
#[derive(Clone, Copy, Debug)]
pub(super) struct Walk {
    group: u32,
    tagged: u32,
}

impl Walk {
    /// A walk that overflows one level past `groups` groups, or `tags`
    /// tagged objects, nested.
    const fn overflowing_past(groups: u32, tags: u32) -> Self {
        Self {
            group: STACK / groups,
            tagged: STACK / tags,
        }
    }
}

/// The check every call makes first.
pub(super) const CHECK: Walk = Walk::overflowing_past(162, 176);
/// `DrawFile_DeclareFonts`' walk.
pub(super) const DECLARE: Walk = Walk::overflowing_past(156, 162);
/// `DrawFile_Render`'s walk.
pub(super) const RENDER: Walk = Walk::overflowing_past(131, 140);

/// What one level took of the stack, given back as it is left.
#[must_use]
#[derive(Debug)]
pub(super) struct Taken(u32);

/// What a walk's open levels have taken of its stack.
#[derive(Debug, Default)]
pub(super) struct Depth(u32);

impl Depth {
    /// One `level` deeper on `walk`'s stack; `None` where it has run out.
    pub(super) fn enter(&mut self, walk: Walk, level: Level) -> Option<Taken> {
        let share = match level {
            Level::Group => walk.group,
            Level::Tagged => walk.tagged,
        };
        self.0 = self.0.checked_add(share).filter(|&used| used <= STACK)?;
        Some(Taken(share))
    }

    /// The level a share was taken for, left: the share given back.
    pub(super) fn leave(&mut self, Taken(share): Taken) {
        self.0 = self.0.saturating_sub(share);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How deep `level` nests on `walk` before its stack runs out.
    fn deepest(walk: Walk, level: Level) -> u32 {
        let mut depth = Depth::default();
        let mut levels = 0;
        while depth.enter(walk, level).is_some() {
            levels += 1;
        }
        levels
    }

    #[test]
    fn each_walk_overflows_one_level_past_where_risc_os_5_30_s_did() {
        for (walk, groups, tags) in [(CHECK, 162, 176), (DECLARE, 156, 162), (RENDER, 131, 140)] {
            assert_eq!(deepest(walk, Level::Group), groups);
            assert_eq!(deepest(walk, Level::Tagged), tags);
        }
    }

    #[test]
    fn a_level_left_gives_its_share_back_and_mixed_levels_add_up() {
        let mut depth = Depth::default();
        let group = depth.enter(RENDER, Level::Group).expect("room");
        let tagged = depth.enter(RENDER, Level::Tagged).expect("room");
        depth.leave(tagged);
        depth.leave(group);
        assert_eq!(depth.0, 0);
        let mut mixed = 0;
        let level = |n: u32| {
            if n.is_multiple_of(2) {
                Level::Group
            } else {
                Level::Tagged
            }
        };
        while depth.enter(RENDER, level(mixed)).is_some() {
            mixed += 1;
        }
        assert!(
            (131..=140).contains(&mixed),
            "between the two kinds' depths: {mixed}"
        );
    }
}
