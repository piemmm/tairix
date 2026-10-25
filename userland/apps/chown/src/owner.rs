//! The owner operand: `[OWNER][:[GROUP]]`, GNU's older `OWNER.GROUP`, and the
//! names in it read against the account and group directories.

use alloc::string::String;

use tairix_abi::fs::FS_OWNER_UNCHANGED;
use tairix_abi::Errno;

use crate::error::ChownError;
use crate::io::Accounts;

/// The ids a change sets, each [`None`] to leave as it is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Owner {
    /// The owning user id.
    pub uid: Option<u32>,
    /// The owning group id.
    pub gid: Option<u32>,
}

impl Owner {
    /// Whether the change names neither id.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.uid.is_none() && self.gid.is_none()
    }

    /// Whether every id this names is the one a node owned by `(uid, gid)`
    /// holds: what `--from` asks of a node.
    #[must_use]
    pub fn matches(self, (uid, gid): (u32, u32)) -> bool {
        self.uid.is_none_or(|want| want == uid) && self.gid.is_none_or(|want| want == gid)
    }
}

/// An owner operand read: the ids it names, and whether GNU's older `.`
/// separated them, which `chown` warns of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The ids the operand names.
    pub owner: Owner,
    /// A `.` rather than a `:` separated the fields.
    pub dotted: bool,
}

/// Read `spec` as GNU's `chown` reads an owner operand.
///
/// `OWNER` names the user, `OWNER:GROUP` both, `:GROUP` the group, and
/// `OWNER:` the user and, for an owner given by name, its login group; an
/// empty operand or `:` names neither. Each field is looked up as a name
/// before it is read as a number, and a leading `+` makes it a number without
/// the lookup. Without a `:`, a `.` separates the fields unless the whole
/// operand names an account.
///
/// # Errors
///
/// [`ChownError::BadUser`] or [`ChownError::BadGroup`] naming `spec`, and
/// [`ChownError::Directory`] when a directory cannot be read.
pub fn resolve(spec: &str, accounts: &dyn Accounts) -> Result<Resolved, ChownError> {
    let (user, group, dotted) = if let Some((user, group)) = spec.split_once(':') {
        (user, Some(group), false)
    } else if let Some((user, group)) = spec.split_once('.') {
        // An account name may hold a `.`, so a dotted operand is a name first.
        let named = accounts
            .user(spec)
            .map_err(|errno| directory(false, errno))?;
        if let Some(account) = named {
            let owner = Owner {
                uid: Some(account.uid),
                gid: None,
            };
            return Ok(Resolved {
                owner,
                dotted: false,
            });
        }
        (user, Some(group), true)
    } else {
        (spec, None, false)
    };
    let owner = fields(spec, user, group, accounts)?;
    Ok(Resolved { owner, dotted })
}

/// The ids `user` and `group` name, `group` [`None`] when no separator
/// followed the owner; a refusal names the whole `spec`.
fn fields(
    spec: &str,
    user: &str,
    group: Option<&str>,
    accounts: &dyn Accounts,
) -> Result<Owner, ChownError> {
    let owner = if user.is_empty() {
        None
    } else {
        let found = lookup(user, |name| accounts.user(name), |account| account.uid)
            .map_err(|errno| directory(false, errno))?;
        Some(found.ok_or_else(|| ChownError::BadUser(String::from(spec)))?)
    };
    let gid = match group {
        None => None,
        // GNU gives a login group only to an owner named by name.
        Some("") => match owner {
            Some(Id::Named(account)) => Some(account.login_group),
            Some(Id::Numbered(_)) | None => None,
        },
        Some(group) => {
            let found = lookup(group, |name| accounts.group(name), |gid| gid)
                .map_err(|errno| directory(true, errno))?;
            let found = found.ok_or_else(|| ChownError::BadGroup(String::from(spec)))?;
            Some(found.value(|gid| gid))
        }
    };
    Ok(Owner {
        uid: owner.map(|owner| owner.value(|account| account.uid)),
        gid,
    })
}

/// The failure to read the account directory, or the group one when
/// `groups`.
fn directory(groups: bool, errno: Errno) -> ChownError {
    ChownError::Directory { groups, errno }
}

/// An id given by number, or the directory entry `T` a name named.
#[derive(Clone, Copy)]
enum Id<T> {
    Named(T),
    Numbered(u32),
}

impl<T> Id<T> {
    /// The id, `id` reading a named entry's.
    fn value(self, id: impl FnOnce(T) -> u32) -> u32 {
        match self {
            Self::Named(entry) => id(entry),
            Self::Numbered(number) => number,
        }
    }
}

/// `text` as a name `find` looks up, else as a number, `id` reading a found
/// entry's id; a leading `+` makes it a number without the lookup.
fn lookup<T: Copy>(
    text: &str,
    find: impl FnOnce(&str) -> Result<Option<T>, Errno>,
    id: impl Fn(T) -> u32,
) -> Result<Option<Id<T>>, Errno> {
    if let Some(digits) = text.strip_prefix('+') {
        return Ok(number(digits).map(Id::Numbered));
    }
    // A directory listing the word that leaves an id alone names nothing
    // that could be given.
    if let Some(entry) = find(text)?.filter(|&entry| id(entry) != FS_OWNER_UNCHANGED) {
        return Ok(Some(Id::Named(entry)));
    }
    Ok(number(text).map(Id::Numbered))
}

/// A decimal id: digits alone, within `u32`, and never the word that leaves
/// an id unchanged.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|&id| id != FS_OWNER_UNCHANGED)
}

#[cfg(test)]
mod tests {
    use super::{resolve, Owner, Resolved};
    use crate::error::ChownError;
    use crate::io::{Account, Accounts};
    use alloc::string::{String, ToString};
    use tairix_abi::Errno;

    /// Directories holding an account and a group named by digits and an
    /// account whose name holds a dot, each account listed with its login
    /// group.
    struct Names {
        unreadable: bool,
    }

    const USERS: &[(&str, u32, u32)] = &[
        ("me", 1000, 1000),
        ("alice", 1001, 50),
        ("j.doe", 1003, 1003),
        ("42", 7, 7),
        ("unchanged", u32::MAX, u32::MAX),
    ];
    const GROUPS: &[(&str, u32)] = &[
        ("me", 1000),
        ("staff", 50),
        ("10", 3),
        ("unchanged", u32::MAX),
    ];
    const READABLE: Names = Names { unreadable: false };

    impl Names {
        fn read(&self) -> Result<(), Errno> {
            if self.unreadable {
                Err(Errno::TimedOut)
            } else {
                Ok(())
            }
        }
    }

    impl Accounts for Names {
        fn user(&self, name: &str) -> Result<Option<Account>, Errno> {
            self.read()?;
            Ok(USERS
                .iter()
                .find(|(listed, ..)| *listed == name)
                .map(|&(_, uid, login_group)| Account { uid, login_group }))
        }

        fn group(&self, name: &str) -> Result<Option<u32>, Errno> {
            self.read()?;
            Ok(GROUPS
                .iter()
                .find(|(listed, _)| *listed == name)
                .map(|&(_, gid)| gid))
        }

        fn user_name(&self, uid: u32) -> Result<Option<String>, Errno> {
            Ok(USERS
                .iter()
                .find(|&&(_, id, _)| id == uid)
                .map(|&(name, ..)| String::from(name)))
        }

        fn group_name(&self, gid: u32) -> Result<Option<String>, Errno> {
            Ok(GROUPS
                .iter()
                .find(|&&(_, id)| id == gid)
                .map(|&(name, _)| String::from(name)))
        }
    }

    fn owner(spec: &str) -> Result<Owner, ChownError> {
        resolve(spec, &READABLE).map(|read| read.owner)
    }

    fn ids(uid: Option<u32>, gid: Option<u32>) -> Owner {
        Owner { uid, gid }
    }

    #[test]
    fn names_and_numbers_name_their_ids() {
        assert_eq!(owner("alice"), Ok(ids(Some(1001), None)));
        assert_eq!(owner("1001"), Ok(ids(Some(1001), None)));
        assert_eq!(owner("alice:staff"), Ok(ids(Some(1001), Some(50))));
        assert_eq!(owner("1001:50"), Ok(ids(Some(1001), Some(50))));
        assert_eq!(owner(":staff"), Ok(ids(None, Some(50))));
        assert_eq!(owner("0:0"), Ok(ids(Some(0), Some(0))));
    }

    /// GNU's manual: a name is resolved before an id is tried, and a leading
    /// `+` forces the id.
    #[test]
    fn a_name_is_read_before_a_number_and_plus_forces_a_number() {
        assert_eq!(owner("42"), Ok(ids(Some(7), None)));
        assert_eq!(owner("+42"), Ok(ids(Some(42), None)));
        assert_eq!(owner(":10"), Ok(ids(None, Some(3))));
        assert_eq!(owner(":+10"), Ok(ids(None, Some(10))));
        assert_eq!(owner("+0:+0"), Ok(ids(Some(0), Some(0))));
    }

    /// GNU's manual: `OWNER:` gives the owner's login group when the owner is
    /// a name, whichever account it names.
    #[test]
    fn owner_colon_gives_a_named_owner_its_login_group_and_a_number_none() {
        assert_eq!(owner("me:"), Ok(ids(Some(1000), Some(1000))));
        assert_eq!(owner("alice:"), Ok(ids(Some(1001), Some(50))));
        assert_eq!(owner("1001:"), Ok(ids(Some(1001), None)));
        assert_eq!(owner("+1000:"), Ok(ids(Some(1000), None)));
    }

    #[test]
    fn an_empty_operand_or_a_lone_colon_names_neither_id() {
        assert_eq!(owner(""), Ok(Owner::default()));
        assert_eq!(owner(":"), Ok(Owner::default()));
    }

    /// GNU's manual: `.` still separates the fields, with a warning, so long
    /// as no ambiguity results.
    #[test]
    fn a_dot_separates_the_fields_only_when_no_account_holds_it() {
        assert_eq!(
            resolve("alice.staff", &READABLE),
            Ok(Resolved {
                owner: ids(Some(1001), Some(50)),
                dotted: true,
            })
        );
        assert_eq!(
            resolve("1001.50", &READABLE),
            Ok(Resolved {
                owner: ids(Some(1001), Some(50)),
                dotted: true,
            })
        );
        assert_eq!(
            resolve("j.doe", &READABLE),
            Ok(Resolved {
                owner: ids(Some(1003), None),
                dotted: false,
            })
        );
        assert_eq!(
            owner("alice:j.doe"),
            Err(ChownError::BadGroup(String::from("alice:j.doe")))
        );
    }

    /// `4294967295` is the word that leaves an id unchanged, so a change
    /// naming it would report a change it never made.
    #[test]
    fn an_unknown_or_unassignable_id_is_refused_naming_the_operand() {
        for spec in [
            "nosuch",
            "+abc",
            "+",
            "-1",
            "4294967295",
            "4294967296",
            "a:b:c",
            "nosuch:staff",
            "unchanged",
        ] {
            assert_eq!(
                owner(spec),
                Err(ChownError::BadUser(String::from(spec))),
                "{spec}"
            );
        }
        for spec in [
            "alice:nosuch",
            ":4294967295",
            "alice:+x",
            "alice:staff:x",
            ":unchanged",
        ] {
            assert_eq!(
                owner(spec),
                Err(ChownError::BadGroup(String::from(spec))),
                "{spec}"
            );
        }
        assert_eq!(
            owner("nosuch").expect_err("refused").to_string(),
            "invalid user: 'nosuch'"
        );
        assert_eq!(
            owner("alice:nosuch").expect_err("refused").to_string(),
            "invalid group: 'alice:nosuch'"
        );
    }

    /// A directory that cannot be read is no proof a name is absent, so even
    /// a number waits on it unless `+` spells it.
    #[test]
    fn an_unreadable_directory_fails_closed_unless_plus_spares_the_lookup() {
        let unreadable = Names { unreadable: true };
        let users = ChownError::Directory {
            groups: false,
            errno: Errno::TimedOut,
        };
        assert_eq!(resolve("0", &unreadable), Err(users.clone()));
        assert_eq!(resolve("a.b", &unreadable), Err(users.clone()));
        assert_eq!(
            resolve(":0", &unreadable),
            Err(ChownError::Directory {
                groups: true,
                errno: Errno::TimedOut,
            })
        );
        assert_eq!(
            resolve("+0:+0", &unreadable).map(|read| read.owner),
            Ok(ids(Some(0), Some(0)))
        );
        assert_eq!(
            users.to_string(),
            "cannot read the account directory: operation timed out"
        );
    }

    #[test]
    fn from_matches_only_the_ids_it_names() {
        assert!(ids(Some(1), None).matches((1, 9)));
        assert!(!ids(Some(1), None).matches((2, 9)));
        assert!(ids(None, Some(9)).matches((1, 9)));
        assert!(ids(Some(1), Some(9)).matches((1, 9)));
        assert!(!ids(Some(1), Some(9)).matches((1, 8)));
        assert!(Owner::default().matches((5, 5)));
    }
}
