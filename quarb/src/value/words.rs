//! The spelled boolean operators, in every language the grammar
//! admits: what a key's text must not be mistaken for, and what the
//! parser reads as `and`, `or` and `not`.

/// The boolean words — the parsed aliases of `&&` / `||` / `!`,
/// which stay canonical — in each language of the rounded family,
/// dotted like the comparison words so that no bare word of any
/// language is a keyword: English `.and.` `.or.` `.not.`, French
/// `.et.` `.ou.` `.non.`, Greek `.και.` `.ή.` `.όχι.` (tonos
/// optional), Latin `.et.` `.vel.` `.non.`, Russian `.и.` `.или.`
/// `.не.`, Spanish `.y.` `.o.` `.no.`. The bare English words stay
/// as sugar (the programmer's habit). On layouts without `&` and
/// `|` the words are the only spelling of conjunction and
/// disjunction.
pub(crate) const AND_WORDS: &[&str] = &[".and.", ".et.", ".και.", ".и.", ".y."];

pub(crate) const OR_WORDS: &[&str] = &[".or.", ".ou.", ".ή.", ".η.", ".vel.", ".или.", ".o."];

pub(crate) const NOT_WORDS: &[&str] = &[".not.", ".non.", ".όχι.", ".οχι.", ".не.", ".no."];

/// Whether a bare word is one of the boolean words — which, unquoted,
/// is never a property name.
pub(crate) fn is_bool_word(word: &str) -> bool {
    AND_WORDS.contains(&word) || OR_WORDS.contains(&word) || NOT_WORDS.contains(&word)
}
