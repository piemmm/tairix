//! Known answers written as hex, for the unit tests.

/// The `N` bytes `text` spells, two hex digits each.
pub(crate) fn hex<const N: usize>(text: &str) -> [u8; N] {
    assert_eq!(
        text.len(),
        2 * N,
        "a {N}-byte answer is {} hex digits",
        2 * N
    );
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("hex digits");
    }
    out
}
