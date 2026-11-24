//! The fonts and encodings `Font$Path` reaches, as RISC OS 5.30 lists them.
//! Each element of the path is read from its Messages file where it has
//! one, which then is all that is known of it, else from a scan of its
//! tree; a font or encoding a later element repeats is the earlier one's.
//! Both lists are in the order of the names shown, ignoring case.

use alloc::vec::Vec;

/// Where an element keeps its encodings, and what a base encoding's file's
/// name opens with.
pub(crate) const ENCODINGS: &[u8] = b"Encodings";
const BASE_MARK: u8 = b'/';

/// A font the path reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Font {
    /// Its identifier: the font's directory below the element.
    pub(crate) identifier: Vec<u8>,
    /// Its name in the current territory, where a Messages file gives it.
    pub(crate) name: Option<Vec<u8>>,
    /// Whether a Messages file makes it its family's default.
    pub(crate) default: bool,
    /// Whether its encoding is its own rather than the alphabet's.
    pub(crate) symbol: bool,
    /// The element it was found in.
    pub(crate) element: usize,
}

impl Font {
    /// The name it is shown by: its name, or else its identifier.
    pub(crate) fn shown(&self) -> &[u8] {
        self.name.as_deref().unwrap_or(&self.identifier)
    }

    fn identifier(&self) -> &[u8] {
        &self.identifier
    }
}

/// An encoding the path reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Encoding {
    /// Its identifier: its file's name below the element's `Encodings`, the
    /// base encodings' leading `/` left off.
    pub(crate) identifier: Vec<u8>,
    /// Its name in the current territory, where a Messages file gives it.
    pub(crate) name: Option<Vec<u8>>,
    /// Whether it is a base encoding, which no list shows.
    pub(crate) base: bool,
    /// The element it was found in.
    pub(crate) element: usize,
}

impl Encoding {
    /// The name it is shown by.
    pub(crate) fn shown(&self) -> &[u8] {
        self.name.as_deref().unwrap_or(&self.identifier)
    }

    fn identifier(&self) -> &[u8] {
        &self.identifier
    }
}

/// What one element of the path holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holding {
    pub(crate) fonts: Vec<Font>,
    pub(crate) encodings: Vec<Encoding>,
}

/// A Messages file's tokens for fonts and encodings.
mod token {
    pub(super) const LANGUAGE_FONT: &[u8] = b"LFont_";
    pub(super) const SYMBOL_FONT: &[u8] = b"Font_";
    pub(super) const ENCODING: &[u8] = b"Encoding_";
    pub(super) const BASE_ENCODING: &[u8] = b"BEncoding_";
}

/// The mark after a font's name that makes it its family's default.
const DEFAULT_MARK: u8 = b'*';

/// What the Messages file `text` of element `element` says it holds, at most
/// `budget` more fonts and encodings. A line is a token, a colon and a value,
/// `#` opening a comment; a line it cannot read is passed over.
pub(crate) fn from_messages(text: &[u8], element: usize, budget: &mut usize) -> Holding {
    let mut holding = Holding::default();
    let mut lines = text.split(|&b| b == b'\n' || b == b'\r');
    while *budget > 0 {
        let Some(line) = lines.next() else {
            break;
        };
        if line.first() == Some(&b'#') {
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (key, value) = (&line[..colon], &line[colon + 1..]);
        let named = |value: &[u8]| (!value.is_empty()).then(|| value.to_vec());
        if let Some(identifier) = key.strip_prefix(token::LANGUAGE_FONT) {
            holding.fonts.push(font(identifier, value, false, element));
        } else if let Some(identifier) = key.strip_prefix(token::SYMBOL_FONT) {
            holding.fonts.push(font(identifier, value, true, element));
        } else if let Some(identifier) = key.strip_prefix(token::BASE_ENCODING) {
            holding.encodings.push(Encoding {
                identifier: identifier.to_vec(),
                name: named(value),
                base: true,
                element,
            });
        } else if let Some(identifier) = key.strip_prefix(token::ENCODING) {
            holding.encodings.push(Encoding {
                identifier: identifier.to_vec(),
                name: named(value),
                base: false,
                element,
            });
        } else {
            continue;
        }
        *budget -= 1;
    }
    holding
}

/// A font a Messages file lists: its name the value less a trailing `*`,
/// which makes it the default, and none where that leaves nothing.
fn font(identifier: &[u8], value: &[u8], symbol: bool, element: usize) -> Font {
    let default = value.last() == Some(&DEFAULT_MARK);
    let name = if default {
        &value[..value.len() - 1]
    } else {
        value
    };
    Font {
        identifier: identifier.to_vec(),
        name: (!name.is_empty()).then(|| name.to_vec()),
        default,
        symbol,
        element,
    }
}

/// An element of the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Element {
    /// The element as the path gives it, its trailing separator included.
    pub(crate) text: Vec<u8>,
    /// Whether it is ResourceFS's font directory.
    pub(crate) rom: bool,
}

/// Every font and encoding the path reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Catalogue {
    /// `Font$Path` as it read when this was made.
    pub(crate) path: Vec<u8>,
    pub(crate) elements: Vec<Element>,
    pub(crate) fonts: Vec<Font>,
    pub(crate) encodings: Vec<Encoding>,
    /// The indices of `fonts` and of `encodings` in the order of their
    /// identifiers, ignoring case.
    fonts_by_identifier: Vec<usize>,
    encodings_by_identifier: Vec<usize>,
}

impl Catalogue {
    /// The catalogue of `path`, its elements holding what `holdings` say in
    /// their order.
    pub(crate) fn of(path: Vec<u8>, elements: Vec<Element>, holdings: Vec<Holding>) -> Self {
        let (mut fonts, mut encodings) = (Vec::new(), Vec::new());
        for holding in holdings {
            fonts.extend(holding.fonts);
            encodings.extend(holding.encodings);
        }
        let (fonts, fonts_by_identifier) = listed(fonts, Font::identifier, Font::shown);
        let (encodings, encodings_by_identifier) =
            listed(encodings, Encoding::identifier, Encoding::shown);
        Self {
            path,
            elements,
            fonts,
            encodings,
            fonts_by_identifier,
            encodings_by_identifier,
        }
    }

    /// The font `identifier` names, ignoring case.
    pub(crate) fn font(&self, identifier: &[u8]) -> Option<&Font> {
        found(
            &self.fonts,
            &self.fonts_by_identifier,
            identifier,
            Font::identifier,
        )
    }

    /// The encodings a list shows: all but the base ones.
    pub(crate) fn listed_encodings(&self) -> impl Iterator<Item = &Encoding> {
        self.encodings.iter().filter(|encoding| !encoding.base)
    }

    /// The base encoding `identifier` names, ignoring case.
    pub(crate) fn base(&self, identifier: &[u8]) -> Option<&Encoding> {
        found(
            &self.encodings,
            &self.encodings_by_identifier,
            identifier,
            Encoding::identifier,
        )
        .filter(|encoding| encoding.base)
    }

    /// The file `encoding` is in.
    pub(crate) fn encoding_file(&self, encoding: &Encoding) -> Vec<u8> {
        let mut file = self.elements[encoding.element].text.clone();
        file.extend_from_slice(ENCODINGS);
        file.push(b'.');
        if encoding.base {
            file.push(BASE_MARK);
        }
        file.extend_from_slice(&encoding.identifier);
        file
    }
}

/// `items` with each identifier its first holder's, ignoring case, in the
/// order of the names they are shown by, ignoring case, and else of `items`;
/// and their indices in the order of their identifiers.
fn listed<T>(
    items: Vec<T>,
    identifier: fn(&T) -> &[u8],
    shown: fn(&T) -> &[u8],
) -> (Vec<T>, Vec<usize>) {
    let mut ranked: Vec<(usize, T)> = items.into_iter().enumerate().collect();
    ranked.sort_unstable_by(|(a_at, a), (b_at, b)| {
        caseless(identifier(a), identifier(b)).then(a_at.cmp(b_at))
    });
    ranked.dedup_by(|(_, later), (_, first)| {
        identifier(later).eq_ignore_ascii_case(identifier(first))
    });
    ranked
        .sort_unstable_by(|(a_at, a), (b_at, b)| caseless(shown(a), shown(b)).then(a_at.cmp(b_at)));
    let items: Vec<T> = ranked.into_iter().map(|(_, item)| item).collect();
    let mut by_identifier: Vec<usize> = (0..items.len()).collect();
    by_identifier.sort_unstable_by(|&a, &b| caseless(identifier(&items[a]), identifier(&items[b])));
    (items, by_identifier)
}

/// The one of `items` whose identifier is `wanted`, ignoring case, by
/// `by_identifier`, their indices in the order of their identifiers.
fn found<'a, T>(
    items: &'a [T],
    by_identifier: &[usize],
    wanted: &[u8],
    identifier: fn(&T) -> &[u8],
) -> Option<&'a T> {
    let at = by_identifier
        .binary_search_by(|&at| caseless(identifier(&items[at]), wanted))
        .ok()?;
    items.get(by_identifier[at])
}

/// `a` against `b`, ignoring case.
pub(crate) fn caseless(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}

/// A family of fonts as a menu shows it: its name, and each member's label
/// with the font it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Family<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) members: Vec<Member<'a>>,
}

/// A member of a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Member<'a> {
    /// What the menu shows: the name past its family's, or none for the
    /// family's own font, which the menu labels as regular.
    pub(crate) label: Option<&'a [u8]>,
    /// The index of its font among the shown names.
    pub(crate) index: usize,
}

impl Family<'_> {
    /// The member a choice of the family alone means: the font its Messages
    /// file makes the default, else its own font, else its first.
    pub(crate) fn chosen(&self, defaults: impl Fn(usize) -> bool) -> Option<Member<'_>> {
        self.members
            .iter()
            .find(|member| defaults(member.index))
            .or_else(|| self.members.iter().find(|member| member.label.is_none()))
            .or_else(|| self.members.first())
            .copied()
    }

    /// Whether its menu has a submenu to open: not if its only member is
    /// its own font.
    pub(crate) fn opens(&self) -> bool {
        !matches!(self.members.as_slice(), [only] if only.label.is_none())
    }
}

/// `names`, in their order, gathered into families by what stands before
/// each one's first full stop, ignoring case, each family where its first
/// member is and named as that member names it.
pub(crate) fn families<'a>(names: impl Iterator<Item = &'a [u8]>) -> Vec<Family<'a>> {
    let mut members: Vec<(&'a [u8], Member<'a>)> = names
        .enumerate()
        .map(|(index, name)| {
            let (family, label) = match name.iter().position(|&b| b == b'.') {
                Some(dot) => (&name[..dot], Some(&name[dot + 1..])),
                None => (name, None),
            };
            (family, Member { label, index })
        })
        .collect();
    members.sort_by(|(a, _), (b, _)| caseless(a, b));
    let mut families: Vec<Family<'a>> = Vec::new();
    for (family, member) in members {
        match families.last_mut() {
            Some(last) if last.name.eq_ignore_ascii_case(family) => last.members.push(member),
            _ => families.push(Family {
                name: family,
                members: alloc::vec![member],
            }),
        }
    }
    families.sort_unstable_by_key(|family| family.members.first().map(|member| member.index));
    families
}

#[cfg(test)]
mod tests;
