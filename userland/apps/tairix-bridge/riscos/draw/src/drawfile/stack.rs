//! How deep a Draw file's groups and tagged objects may nest before the
//! supervisor stack a walk of them runs on overflows, as RISC OS 5.30's
//! DrawFile overflows its own when called from BASIC: each level of a walk
//! takes its kind's share of the stack, so mixed nesting adds up as a stack
//! does, and no file nests deeper than the host's own stack can follow. Every
//! walk descends into a group's objects and a tagged object's object alike.

use tairix_drawing::riscos::format::{Misframed, Object, Objects, GROUP_OBJECTS, TAGGED_OBJECT};
use tairix_riscos_kernel::os::{Fail, Os};

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

/// What the levels a walk is within have taken of its stack: a level's
/// own, so leaving it gives its share back.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Depth(u32);

impl Depth {
    /// One `level` deeper on `walk`'s stack; `None` where it has run out.
    pub(super) fn enter(self, walk: Walk, level: Level) -> Option<Self> {
        let share = match level {
            Level::Group => walk.group,
            Level::Tagged => walk.tagged,
        };
        self.0
            .checked_add(share)
            .filter(|&used| used <= STACK)
            .map(Self)
    }

    /// One `level` deeper on `walk`'s stack, or the supervisor stack's
    /// overflow.
    pub(super) fn deeper(self, os: &mut Os<'_>, walk: Walk, level: Level) -> Result<Self, Fail> {
        self.enter(walk, level)
            .ok_or_else(|| Fail::Unwind(os.overflowed()))
    }
}

/// The objects `group` holds after its name, where they lie in the file.
pub(super) fn group_objects(group: Object<'_>) -> Objects<'_> {
    let objects = group.bytes.get(GROUP_OBJECTS..).unwrap_or(&[]);
    Objects::new(objects, group.at + GROUP_OBJECTS)
}

/// The object `tagged` wraps after its tag, where it lies in the file; none
/// where nothing follows the tag.
pub(super) fn tagged_object(tagged: Object<'_>) -> Option<Result<Object<'_>, Misframed>> {
    let wrapped = tagged.bytes.get(TAGGED_OBJECT..).unwrap_or(&[]);
    Objects::new(wrapped, tagged.at + TAGGED_OBJECT).next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How deep `level` nests on `walk` before its stack runs out.
    fn deepest(walk: Walk, level: Level) -> u32 {
        let mut depth = Depth::default();
        let mut levels = 0;
        while let Some(deeper) = depth.enter(walk, level) {
            depth = deeper;
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
        let top = Depth::default();
        let group = top.enter(RENDER, Level::Group).expect("room");
        let both = group.enter(RENDER, Level::Tagged).expect("room");
        assert_eq!(both.0, RENDER.group + RENDER.tagged);
        let sibling = top.enter(RENDER, Level::Tagged).expect("room");
        assert_eq!(sibling.0, RENDER.tagged, "the level left took nothing");
        let mut depth = top;
        let mut mixed = 0;
        let level = |n: u32| {
            if n.is_multiple_of(2) {
                Level::Group
            } else {
                Level::Tagged
            }
        };
        while let Some(deeper) = depth.enter(RENDER, level(mixed)) {
            depth = deeper;
            mixed += 1;
        }
        assert!(
            (131..=140).contains(&mixed),
            "between the two kinds' depths: {mixed}"
        );
    }
}
