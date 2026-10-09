//! Quarb's time, on its own: the temporal fragment's instants and
//! durations ([`temporal`]: ISO 8601 and compact forms, spans,
//! `strptime` / `strftime` under a locale's names), periods and
//! grains ([`period`]), time expressions in eleven languages
//! ([`timex`]), and the calendar names behind them
//! ([`datenames`]). Dependency-free apart from `regex`; the engine
//! (`quarb`) re-exports these modules at their old paths.

pub mod datenames;
pub mod period;
pub mod temporal;
pub mod timex;
mod timexdata;

/// The language of a BCP 47 tag (or a POSIX locale name): `ru-RU`,
/// `ru_RU.UTF-8`, `ru@petr1708` all answer `ru`, lower-cased. The
/// one tag resolution every table shares; an empty tag is the
/// empty language, which the tables read as English.
pub fn language(tag: &str) -> String {
    tag.trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}
