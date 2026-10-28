//! [`flags!`](crate::flags!): a set of bit flags over one unsigned integer.

/// A set of bit flags over one unsigned integer, as a field carries it: the
/// named flags, the empty set, and setting, clearing and testing them.
///
/// ```
/// tairix_util::flags! {
///     /// What a door allows.
///     pub struct Door(u8) {
///         /// It opens.
///         const OPEN = 1;
///         /// It locks.
///         const LOCK = 2;
///     }
/// }
///
/// let door = Door::OPEN.with(Door::LOCK);
/// assert!(door.contains(Door::LOCK));
/// assert!(!door.without(Door::LOCK).contains(Door::LOCK));
/// assert_eq!(Door::from_bits(door.bits()), door);
/// ```
#[macro_export]
macro_rules! flags {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident($bits:ty) {
            $( $(#[$flag_meta:meta])* const $flag:ident = $value:expr; )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
        $vis struct $name($bits);

        impl $name {
            $( $(#[$flag_meta])* pub const $flag: Self = Self($value); )*

            /// No flag set.
            pub const NONE: Self = Self(0);

            /// The flags of raw value `bits`.
            #[must_use]
            pub const fn from_bits(bits: $bits) -> Self {
                Self(bits)
            }

            /// The raw value.
            #[must_use]
            pub const fn bits(self) -> $bits {
                self.0
            }

            /// These flags with `other` set too.
            #[must_use]
            pub const fn with(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// These flags with `other` clear.
            #[must_use]
            pub const fn without(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }

            /// Whether every flag of `other` is set.
            #[must_use]
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }
    };
}
