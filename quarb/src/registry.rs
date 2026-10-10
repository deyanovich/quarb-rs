//! The function registry: every name a pipeline may call, with the
//! positions it stands in, the family that evaluates it, and a line
//! of documentation — one table behind the parser's validation, the
//! executor's dispatch, completion and highlighting.
//!
//! A name is *scalar* when it works per capsa on the plain pipe
//! (`| upper`), and an *aggregate* when it reads the whole context
//! on `@|`. The aggregates divide by what they do with the capsae:
//! a [`Agg::Reducing`] aggregate folds them to a value (and so also
//! works per capsa on a group's list members: `@| group(::k) |
//! mean`); a [`Agg::Context`] stage reads the whole context only and
//! never takes the plain pipe; a [`Agg::Keyed`] aggregate reorders or
//! filters the capsae themselves by per-capsa keys; an
//! [`Agg::Association`] reduces two per-capsa expressions to one
//! number. A few names are both scalar and aggregate (`json` renders
//! each topic, or the whole context as one document).

use crate::ast::FnCall;

/// What an aggregate does with the capsae.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    /// Folds the capsae to a value; per capsa on a list topic too.
    Reducing,
    /// Reads the whole context only: `@|` and never `|`.
    Context,
    /// Reorders or filters the capsae by per-capsa keys.
    Keyed,
    /// Reduces two per-capsa expressions to one number; an
    /// *agreement* measure reads them as labels, not numbers.
    Association { agreement: bool },
}

/// The argument check the parser runs on a call to this name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// No shape beyond the generic argument rules.
    None,
    /// The record convention (`key = value` fields, auto-named
    /// projections).
    Record,
    /// A keyed aggregate: expression keys, the count of `top` /
    /// `bottom`, the collation words of `sort`, `group`'s keys.
    Keyed,
    /// A whole-context stage's own shape: `window` / `shift` (an
    /// offset range or distance, then a key), the sizes of
    /// `combinations` / `pairs` / `triples`, the columns of `csv` /
    /// `table`, `number`, `timeline`, `outline`, `zscore`.
    Stage,
    /// `decode` / `dec`: one reversible scheme name.
    Decode,
    /// The statistics round: `sample` for the dispersion measures,
    /// two expressions for an association.
    Stat,
}

/// One registered function.
#[derive(Debug, Clone, Copy)]
pub struct FnSpec {
    pub name: &'static str,
    /// Works per capsa on the plain pipe.
    pub scalar: bool,
    /// Works on `@|`, and how.
    pub agg: Option<Agg>,
    /// The parser's argument check.
    pub check: Check,
    /// One line for people: completion, the manual.
    pub doc: &'static str,
}

const fn scalar(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: true,
        agg: None,
        check: Check::None,
        doc,
    }
}

const fn reducing(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: false,
        agg: Some(Agg::Reducing),
        check: Check::None,
        doc,
    }
}

const fn context(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: false,
        agg: Some(Agg::Context),
        check: Check::None,
        doc,
    }
}

const fn keyed(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: false,
        agg: Some(Agg::Keyed),
        check: Check::Keyed,
        doc,
    }
}

const fn assoc(name: &'static str, agreement: bool, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: false,
        agg: Some(Agg::Association { agreement }),
        check: Check::Stat,
        doc,
    }
}

const fn both(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        scalar: true,
        agg: Some(Agg::Reducing),
        check: Check::None,
        doc,
    }
}

const fn with_check(spec: FnSpec, check: Check) -> FnSpec {
    FnSpec { check, ..spec }
}

/// Every function, in the order the manual lists them.
pub const SPECS: &[FnSpec] = &[
    // Text.
    scalar("upper", "the text in upper case"),
    scalar("lower", "the text in lower case"),
    scalar("title", "the text with each word capitalized"),
    scalar("trim", "the text without leading and trailing whitespace"),
    scalar("ltrim", "the text without leading whitespace"),
    scalar("rtrim", "the text without trailing whitespace"),
    scalar("alpha", "the letters of the text"),
    scalar("digit", "the digits of the text"),
    scalar("alnum", "the letters and digits of the text"),
    scalar("cc", "the count of codepoints"),
    scalar("bc", "the count of UTF-8 bytes"),
    scalar("wc", "the count of words"),
    scalar("lc", "the count of lines"),
    scalar("sc", "the count of sentences"),
    scalar("lines", "the lines of the text, one capsa each"),
    scalar("words", "the words of the text, one capsa each"),
    with_check(
        both(
            "ngrams",
            "the word n-grams of the text: ngrams(N); as an aggregate, the \
             adjacent N-windows of the stream, full windows only",
        ),
        Check::Stage,
    ),
    scalar("levenshtein", "the edit distance to the argument"),
    scalar(
        "jaro_winkler",
        "the Jaro–Winkler similarity to the argument, 0 to 1",
    ),
    scalar(
        "tokens",
        "the corpus tier's tokens of the text as a list; tokens(word) the \
         word class alone",
    ),
    scalar("casefold", "Unicode case folding (ß to ss, final sigma)"),
    scalar(
        "deaccent",
        "the text without combining marks (NFD, marks stripped); deaccent(ru) \
         folds ё too",
    ),
    scalar(
        "stem",
        "the Snowball stem under a language: stem(en), stem(ru), …",
    ),
    scalar("s", "regex substitution: s/pattern/replacement/mods"),
    scalar(
        "split",
        "the pieces between a separator (whitespace unless given)",
    ),
    scalar("substr", "the codepoints of a range: substr(a..b)"),
    scalar("pad", "the text padded on the right to a width"),
    scalar("lpad", "the text padded on the left to a width"),
    scalar("rpad", "the text padded on the right to a width"),
    scalar("repeat", "the text repeated N times"),
    scalar("indent", "every line indented by N spaces"),
    scalar(
        "sentences",
        "the sentences of the text, under the session's bonds",
    ),
    scalar("default", "the argument where the topic is null"),
    // Unicode, stress, transliteration, spelling.
    scalar("nfc", "the text in Unicode NFC"),
    scalar("nfd", "the text in Unicode NFD"),
    scalar("vowels", "the vowel count of the word"),
    scalar("stress", "the word with the stress mark on vowel N"),
    scalar("stress_at", "the index of the stressed vowel"),
    scalar("unstress", "the word without stress marks"),
    scalar("accent", "the stress mark rendered in a style"),
    scalar("translit", "the text transliterated under a scheme"),
    scalar(
        "modernize",
        "historical orthography modernized under a table",
    ),
    scalar("gc", "the count of grapheme clusters"),
    // Numbers.
    FnSpec {
        name: "number",
        scalar: true,
        agg: Some(Agg::Context),
        check: Check::Stage,
        doc: "the numeric reading of text (per capsa), or the running number of each capsa (@|)",
    },
    scalar("round", "the number rounded to N digits"),
    scalar("floor", "the number rounded down"),
    scalar("ceil", "the number rounded up"),
    scalar("abs", "the absolute value"),
    scalar("log", "the natural logarithm"),
    scalar("log2", "the base-2 logarithm"),
    scalar("log10", "the base-10 logarithm"),
    scalar("exp", "e to the power of the number"),
    scalar("sqrt", "the square root"),
    scalar("pow", "the number to the power of the argument"),
    scalar(
        "loglik",
        "the log-likelihood keyness of a count against a reference",
    ),
    scalar(
        "chi2",
        "the chi-squared keyness of a count against a reference",
    ),
    scalar("mi", "the mutual-information collocation measure"),
    scalar("t_score", "the t-score collocation measure"),
    scalar("log_dice", "the logDice collocation measure"),
    scalar("logdice", "log_dice, alias"),
    scalar("tscore", "t_score, alias"),
    scalar("dice", "the Dice coefficient of a collocation"),
    scalar("haversine", "the great-circle distance to a point"),
    scalar("quantity", "the text read as a quantity with a unit"),
    scalar("convert", "the quantity converted to a unit"),
    // Serializations.
    both("json", "the topic (per capsa) or the context (@|) as JSON"),
    both(
        "jsonl",
        "the topic (per capsa) or the context (@|) as JSON Lines",
    ),
    both("kaiv", "the topic (per capsa) or the context (@|) as kaiv"),
    scalar("link", "the node's link address"),
    scalar("xml", "the node as XML"),
    scalar("markdown", "the node as Markdown"),
    scalar("html", "the node as HTML"),
    scalar("atrep", "the node as atrep text"),
    scalar("conllu", "the node as CoNLL-U"),
    scalar("bibref", "the reference parsed into its fields"),
    with_check(
        scalar("record", "retired: the record is %(…)"),
        Check::Record,
    ),
    with_check(scalar("rec", "retired: the record is %(…)"), Check::Record),
    // The temporal fragment.
    scalar("datetime", "the instant read from text"),
    scalar("epoch", "the instant as seconds since the epoch"),
    scalar("isoformat", "the instant in ISO 8601"),
    scalar("year", "the instant's year"),
    scalar("month", "the instant's month"),
    scalar("day", "the instant's day of the month"),
    scalar("hour", "the instant's hour"),
    scalar("minute", "the instant's minute"),
    scalar("second", "the instant's second"),
    scalar("weekday", "the instant's day of the week"),
    scalar("date", "the instant's calendar date"),
    scalar("isodate", "the instant's date in ISO 8601"),
    scalar("isomonth", "the instant's year and month in ISO 8601"),
    scalar("isoweek", "the instant's ISO week"),
    scalar("seconds", "the duration in seconds"),
    scalar("minutes", "the duration in minutes"),
    scalar("hours", "the duration in hours"),
    scalar("days", "the duration in days"),
    scalar("duration", "the duration read from text"),
    scalar("td", "duration, short"),
    scalar("strptime", "the instant read under a format and a locale"),
    scalar("tp", "strptime, short"),
    scalar(
        "strftime",
        "the instant written under a format and a locale",
    ),
    scalar("tfmt", "strftime, short"),
    scalar("timex", "the time expressions of the text"),
    scalar(
        "time_anchor",
        "the time expression resolved against an anchor",
    ),
    scalar("period", "the period a time expression denotes"),
    scalar("grain", "the granularity of a time expression"),
    // Encodings and the shell.
    scalar("sha256", "the SHA-256 digest, hex"),
    scalar("base64", "the text in base64"),
    scalar("base64url", "the text in URL-safe base64"),
    scalar("base32", "the text in base32"),
    scalar("crockford32", "the text in Crockford base32"),
    scalar("hex", "the text in hexadecimal"),
    with_check(
        scalar("decode", "the text decoded from a scheme"),
        Check::Decode,
    ),
    with_check(scalar("dec", "decode, short"), Check::Decode),
    scalar(
        "sh",
        "the topic through a shell command (needs --allow-shell)",
    ),
    // Reductions.
    reducing("count", "how many"),
    reducing("sum", "the sum"),
    reducing("product", "the product"),
    reducing("min", "the least"),
    reducing("max", "the greatest"),
    reducing("mean", "the arithmetic mean"),
    reducing("avg", "mean, alias"),
    reducing("median", "the median"),
    reducing("percentile", "the Nth percentile"),
    with_check(
        reducing(
            "std",
            "the standard deviation (sample with the word sample)",
        ),
        Check::Stat,
    ),
    with_check(
        reducing("var", "the variance (sample with the word sample)"),
        Check::Stat,
    ),
    with_check(reducing("stddev", "std, alias"), Check::Stat),
    with_check(reducing("variance", "var, alias"), Check::Stat),
    reducing(
        "entropy",
        "the Shannon entropy, bits: of a stream of counts, or of the \
         distribution of a stream of values",
    ),
    reducing("ttr", "the type-token ratio: distinct values over all"),
    reducing(
        "hapax",
        "the values that occur once, in first-occurrence order",
    ),
    with_check(
        reducing(
            "skipgrams",
            "the n-grams with up to k skipped positions: skipgrams(n; k)",
        ),
        Check::Stage,
    ),
    reducing(
        "jaccard",
        "the Jaccard similarity of the stream's distinct values to a list",
    ),
    reducing("yule_k", "Yule's K over a stream of counts"),
    reducing(
        "mtld",
        "the measure of textual lexical diversity over tokens",
    ),
    with_check(
        reducing("bigrams", "the adjacent pairs of the stream (ngrams(2))"),
        Check::Stage,
    ),
    with_check(
        reducing("trigrams", "the adjacent triples of the stream (ngrams(3))"),
        Check::Stage,
    ),
    with_check(
        reducing(
            "frequencies",
            "each distinct value with its count, most frequent first, as \
             records (value, n); frequencies(K) keeps the top K",
        ),
        Check::Stage,
    ),
    with_check(
        reducing("combinations", "every combination of N capsae"),
        Check::Stage,
    ),
    with_check(reducing("pairs", "every pair of capsae"), Check::Stage),
    with_check(reducing("triples", "every triple of capsae"), Check::Stage),
    with_check(
        reducing(
            "sort",
            "the capsae sorted by value (sort(LOCALE), sort(atergo))",
        ),
        Check::Keyed,
    ),
    reducing("unique", "the distinct values"),
    reducing("reverse", "the capsae in reverse order"),
    reducing("first", "the first N (one unless given)"),
    reducing("last", "the last N (one unless given)"),
    reducing("join", "the values joined with a separator"),
    // Associations.
    assoc("corr", false, "Pearson correlation of two expressions"),
    assoc(
        "spearman",
        false,
        "Spearman rank correlation of two expressions",
    ),
    assoc("cosine", false, "cosine similarity of two expressions"),
    assoc("delta", false, "Burrows's Delta between two expressions"),
    assoc("dp", false, "Gries's deviation of proportions"),
    assoc(
        "kappa",
        true,
        "Cohen's kappa agreement of two label expressions",
    ),
    assoc(
        "precision",
        true,
        "precision of a prediction against a truth",
    ),
    assoc("recall", true, "recall of a prediction against a truth"),
    assoc("f1", true, "the F1 score of a prediction against a truth"),
    // Whole-context stages.
    context("ungroup", "the groups' members as the context again"),
    with_check(
        context(
            "window",
            "each capsa with its neighbours: window(-2..0), window(3)",
        ),
        Check::Stage,
    ),
    with_check(
        context("shift", "each capsa's value from N capsae back"),
        Check::Stage,
    ),
    with_check(
        context("zscore", "each value standardized over the context"),
        Check::Stage,
    ),
    with_check(
        context("outline", "the context as a nested outline"),
        Check::Stage,
    ),
    with_check(
        context("timeline", "the context ordered on its time anchors"),
        Check::Stage,
    ),
    with_check(
        context("csv", "the records as CSV (closing: the tool renders it)"),
        Check::Stage,
    ),
    with_check(
        context(
            "table",
            "the records as an aligned table (closing: the tool renders it)",
        ),
        Check::Stage,
    ),
    context(
        "encoding",
        "the output encoding (closing: the tool applies it)",
    ),
    // Keyed aggregates.
    keyed("sort_by", "the capsae sorted by keys"),
    keyed("unique_by", "the first capsa per distinct key"),
    keyed("min_by", "the capsa with the least key"),
    keyed("max_by", "the capsa with the greatest key"),
    keyed("top", "the N capsae with the greatest key"),
    keyed("bottom", "the N capsae with the least key"),
    keyed("group", "the capsae grouped by keys"),
];

/// The registered function called `name`.
pub fn spec(name: &str) -> Option<&'static FnSpec> {
    SPECS.iter().find(|s| s.name == name)
}

/// The functions that may stand in a position, for enumeration
/// (completion, the manual).
pub fn names(scalar: bool, agg: Option<Agg>) -> impl Iterator<Item = &'static str> {
    SPECS
        .iter()
        .filter(move |s| (scalar && s.scalar) || (agg.is_some() && s.agg == agg))
        .map(|s| s.name)
}

/// The parser's argument check for a call, by name.
pub fn check(call: &FnCall) -> Check {
    spec(&call.name).map_or(Check::None, |s| s.check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in SPECS {
            assert!(seen.insert(s.name), "{} registered twice", s.name);
            assert!(s.scalar || s.agg.is_some(), "{} stands nowhere", s.name);
            assert!(!s.doc.is_empty(), "{} has no doc", s.name);
        }
    }
}
