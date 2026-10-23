//! The command registry: every name a pipeline may call, with the
//! positions it stands in, the family that evaluates it, and a line
//! of documentation — one table behind the parser's validation, the
//! executor's dispatch, completion and highlighting.
//!
//! A name is *scalar* when it works per capsa on the plain pipe
//! (`| upper`), and an *aggregate* when it reads the whole context
//! on `@|`. The aggregates divide by what they do with the capsae:
//! a [`Agg::Reducing`] aggregate folds them to a value (and so also
//! works per capsa on a group's list members: `@| group(::k) |
//! mean`); a [`Agg::Context`] command reads the whole context only and
//! never takes the plain pipe; a [`Agg::Keyed`] aggregate reorders or
//! filters the capsae themselves by per-capsa keys; an
//! [`Agg::Association`] reduces two per-capsa expressions to one
//! number. A few names are both scalar and aggregate (`json` renders
//! each topic, or the whole context as one document).

use crate::syntax::ast::FnCall;

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
    /// A whole-context command's own shape: `window` / `shift` (an
    /// offset range or distance, then a key), the sizes of
    /// `combinations` / `pairs` / `triples`, the columns of `csv` /
    /// `table`, `number`, `timeline`, `outline`, `zscore`.
    Command,
    /// `decode` / `dec`: one reversible scheme name.
    Decode,
    /// The statistics round: `sample` for the dispersion measures,
    /// two expressions for an association.
    Stat,
}

/// One registered command.
/// The layer a command answers from: a value (the calculator has it)
/// or the thread's position (the graph's; a bare value has nothing to
/// give it). A value command on a thread standing on a node reads the
/// node's scalar when it has no topic, which is the graph's business,
/// not the command's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Value,
    Node,
}

/// What a scalar answers to a null topic (spec: the inventory's
/// rule — a null topic has no coercion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnNull {
    /// Null: the reading has nothing to read.
    Null,
    /// Nothing: an expander yields no capsa.
    Nothing,
    /// The command reads null on purpose (`default`, `type`, the
    /// serializers) and answers in its own arm.
    Keep,
}

#[derive(Debug, Clone, Copy)]
pub struct FnSpec {
    pub name: &'static str,
    /// Where the command's answer comes from.
    pub layer: Layer,
    /// Works per capsa on the plain pipe.
    pub scalar: bool,
    /// Works on `@|`, and how.
    pub agg: Option<Agg>,
    /// The parser's argument check.
    pub check: Check,
    /// One line for people: completion, the manual.
    pub doc: &'static str,
    /// The answer to a null topic on the plain pipe.
    pub null: OnNull,
    /// The options the command declares, as a utility's synopsis
    /// lists them (`-i`, `--locale`); any other option in its
    /// segment is refused by name. No built-in declares one yet:
    /// the syntax stands empty for the shell-function round.
    pub options: &'static [&'static str],
}

const fn scalar(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: true,
        agg: None,
        check: Check::None,
        doc,
        null: OnNull::Null,
        options: &[],
    }
}

/// The command's answer to a null topic, where it is not null.
const fn with_null(spec: FnSpec, null: OnNull) -> FnSpec {
    FnSpec { null, ..spec }
}

/// Whether the command takes `--of=expr` (`-o=expr`): every
/// reducing aggregate of the inventory (spec: Aggregating Pipe,
/// draft 164) — the expression is read on each item of the input in
/// turn, and the reduction runs over the results.
pub fn takes_of(name: &str) -> bool {
    spec(name).is_some_and(|s| matches!(s.agg, Some(Agg::Reducing)) && !s.scalar)
}

const fn reducing(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: false,
        agg: Some(Agg::Reducing),
        check: Check::None,
        doc,
        null: OnNull::Keep,
        options: &[],
    }
}

const fn context(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: false,
        agg: Some(Agg::Context),
        check: Check::None,
        doc,
        null: OnNull::Keep,
        options: &[],
    }
}

/// A closing renderer: an aggregate the tool takes off and renders
/// (`@| csv`, `@| pretty`), and a scalar command after a value
/// (`| pretty` after a table).
const fn rendering(name: &'static str, doc: &'static str) -> FnSpec {
    let mut s = context(name, doc);
    s.scalar = true;
    s
}

const fn keyed(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: false,
        agg: Some(Agg::Keyed),
        check: Check::Keyed,
        doc,
        null: OnNull::Keep,
        options: &[],
    }
}

const fn assoc(name: &'static str, agreement: bool, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: false,
        agg: Some(Agg::Association { agreement }),
        check: Check::Stat,
        doc,
        null: OnNull::Keep,
        options: &[],
    }
}

const fn both(name: &'static str, doc: &'static str) -> FnSpec {
    FnSpec {
        name,
        layer: Layer::Value,
        scalar: true,
        agg: Some(Agg::Reducing),
        check: Check::None,
        doc,
        null: OnNull::Null,
        options: &[],
    }
}

/// The options a command declares beyond `--of` (draft 164).
const fn with_options(spec: FnSpec, options: &'static [&'static str]) -> FnSpec {
    FnSpec { options, ..spec }
}

const fn with_check(spec: FnSpec, check: Check) -> FnSpec {
    FnSpec { check, ..spec }
}

/// A command that reads the thread's position: the graph's.
const fn on_node(spec: FnSpec) -> FnSpec {
    FnSpec {
        layer: Layer::Node,
        ..spec
    }
}

/// Every command, in the order the manual lists them.
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
    with_null(
        scalar("lines", "the lines of the text, one capsa each"),
        OnNull::Nothing,
    ),
    with_null(
        scalar("words", "the words of the text, one capsa each"),
        OnNull::Nothing,
    ),
    with_check(
        with_null(
            both(
                "ngrams",
                "the word n-grams of the text: ngrams(N); as an aggregate, the \
             adjacent N-windows of the stream, full windows only",
            ),
            OnNull::Nothing,
        ),
        Check::Command,
    ),
    scalar("levenshtein", "the edit distance to the argument"),
    scalar(
        "jaro_winkler",
        "the Jaro–Winkler similarity to the argument, 0 to 1",
    ),
    with_null(
        scalar(
            "tokens",
            "the corpus tier's tokens of the text as a list; tokens(word) the \
         word class alone",
        ),
        OnNull::Nothing,
    ),
    scalar("casefold", "Unicode case folding (ß to ss, final sigma)"),
    scalar(
        "deaccent",
        "the text without the accents of a language, its letters kept: \
         deaccent(fr); bare, every combining mark off (= strip)",
    ),
    scalar("strip", "the text without any combining mark"),
    scalar(
        "stem",
        "the Snowball stem under a language: stem(en), stem(ru), …",
    ),
    scalar(
        "s",
        "regex substitution: s/pattern/replacement/mods (mods from g, i, m, s, x)",
    ),
    with_null(
        scalar(
            "split",
            "the pieces between a separator (whitespace unless given)",
        ),
        OnNull::Nothing,
    ),
    scalar("substr", "the codepoints of a range: substr(a..b)"),
    scalar("pad", "the text padded on the right to a width"),
    scalar("lpad", "the text padded on the left to a width"),
    scalar("rpad", "the text padded on the right to a width"),
    scalar("repeat", "the text repeated N times"),
    scalar("indent", "every line indented by N spaces"),
    with_null(
        scalar(
            "sentences",
            "the sentences of the text, under the session's bonds",
        ),
        OnNull::Nothing,
    ),
    with_null(
        scalar("default", "the argument where the topic is null"),
        OnNull::Keep,
    ),
    with_check(
        with_options(
            scalar(
                "map",
                "the body applied to every value one level deep — the cells of a table, the items of a list, \
                 the fields of a record, each the topic of the body in turn — and the structure answered \
                 with the results in place (a table re-types its columns); --to=:col, or a parenthesised \
                 list of columns, restricts the mapping",
            ),
            &["--to"],
        ),
        Check::Command,
    ),
    // (`project` is retired at 0.42.0: the projection operator, see
    // `retired_command` in the parser)
    // Unicode, stress, transliteration, spelling.
    scalar("nfc", "the text in Unicode NFC"),
    scalar("nfd", "the text in Unicode NFD"),
    scalar("vowels", "the vowel count of the word"),
    scalar("stress", "the word with the stress mark on vowel N"),
    scalar("stress_at", "the index of the stressed vowel"),
    scalar("unstress", "the word without stress marks"),
    scalar("accent", "the stress mark rendered in a style"),
    scalar(
        "translit",
        "the text transliterated under a table: the script's ISO table by \
         default; gost, scientific, bgn, passport, ala, slug, iso9-cyrl, \
         gost-cyrl, uk, bg, be, kk, mn, sr-latn, sr-cyrl; elot, betacode; \
         din31635, buckwalter, heb; ka; iast, hunterian; hepburn, pinyin, wade, \
         rr, mr; de, mrz, ascii",
    ),
    scalar(
        "modernize",
        "historical orthography modernized under a table",
    ),
    scalar("gc", "the count of grapheme clusters"),
    // Numbers.
    FnSpec {
        name: "number",
        null: OnNull::Null,
        layer: Layer::Value,
        scalar: true,
        agg: Some(Agg::Context),
        check: Check::Command,
        doc: "the numeric reading of text (per capsa), or the running number of each capsa (@|)",
        options: &[],
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
    scalar("exchange", "the currency exchanged by a rates record"),
    scalar(
        "scale",
        "the quantity in the prefix of its unit that brings the magnitude to one or more",
    ),
    scalar(
        "dfmt",
        "the duration as mixed span text, in a locale's designations with (LOCALE), or ISO-8601 with (iso)",
    ),
    scalar(
        "unit",
        "the written unit as unit text; with (U), the topic in U",
    ),
    scalar(
        "magnitude",
        "the number in the written unit; with (U), the number in U",
    ),
    scalar(
        "dimension",
        "the dimension of a quantity over the base dimensions",
    ),
    // The two readings of a table (spec: Command Libraries, "A
    // library value") and the reshapes that need no algorithm.
    with_null(
        both(
            "@",
            "the row reading: a library value as its rows, a record of lists as a list of records",
        ),
        OnNull::Nothing,
    ),
    both(
        "%",
        "the column reading: a library value as its columns, a list of records as a record of lists",
    ),
    both(
        "transpose",
        "a list of lists with its rows and columns exchanged",
    ),
    both(
        "#",
        "the table reading: a list of records as a frame, a list of lists as an array, a list of values as a sample",
    ),
    with_null(
        both(
            "rows",
            "the row reading of a table or a library value, as '| @'",
        ),
        OnNull::Nothing,
    ),
    both(
        "columns",
        "the column reading of a table or a library value, as '| %'",
    ),
    both(
        "header",
        "a table's header, the list of column names (null on the array face)",
    ),
    both("dim", "a table's shape, @(rows; columns)"),
    both(
        "keys",
        "a record's field names as a list, in order; a table's header",
    ),
    both(
        "fields",
        "the field projections of the topic as a list (:a): a record's fields, a table's columns, a node's core fields",
    ),
    both(
        "values",
        "a record's values as a list, in order; a table's rows as an array, columns and types kept",
    ),
    with_null(
        both(
            "flatten",
            "every nested list opened to any depth (flatten(n): n levels), scalars and nulls in order; a table's cells row by row",
        ),
        OnNull::Keep,
    ),
    with_null(
        both(
            "type",
            "a table's column types, one word per column: boolean, integer, float, instant, duration, period, quantity, text",
        ),
        OnNull::Keep,
    ),
    both(
        "[]",
        "a row of a table by position, or a slice of its rows (written '| [n]', '| [a..b]')",
    ),
    both(
        "cell",
        "one cell of a table: cell(i; j), the row by position, the column by position or name",
    ),
    both("row-to-header", "the first row lifted into the header"),
    both(
        "header-to-row",
        "the header lowered into the first row (as text), the table then an array",
    ),
    both(
        "pivot",
        "long to wide: pivot(row; column; value) makes one record per row key with a field per column key",
    ),
    FnSpec {
        name: "melt",
        null: OnNull::Nothing,
        layer: Layer::Value,
        scalar: true,
        agg: Some(Agg::Reducing),
        check: Check::None,
        doc: "wide to long: melt(id; …) makes one record per other field, as name and value",
        options: &["--name", "--value"],
    },
    // Serializations.
    with_null(
        both("json", "the topic (per capsa) or the context (@|) as JSON"),
        OnNull::Keep,
    ),
    with_null(
        both(
            "jsonl",
            "the topic (per capsa) or the context (@|) as JSON Lines",
        ),
        OnNull::Keep,
    ),
    with_null(
        both("kaiv", "the topic (per capsa) or the context (@|) as kaiv"),
        OnNull::Keep,
    ),
    on_node(scalar("link", "the node's link address")),
    scalar("xml", "the node as XML"),
    scalar("markdown", "the node as Markdown"),
    scalar("html", "the node as HTML"),
    scalar("atrep", "the node as atrep text"),
    scalar("conllu", "the node as CoNLL-U"),
    scalar("bibref", "the reference parsed into its fields"),
    with_check(
        with_null(
            scalar("record", "retired: the record is %(…)"),
            OnNull::Keep,
        ),
        Check::Record,
    ),
    with_check(
        with_null(scalar("rec", "retired: the record is %(…)"), OnNull::Keep),
        Check::Record,
    ),
    // The temporal fragment.
    scalar("datetime", "the instant read from text"),
    scalar("epoch", "the instant as seconds since the epoch"),
    scalar("truncate", "the instant floored to a multiple of a fixed-length duration from the epoch"),
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
    scalar(
        "duration",
        "the duration read from text, or from one instant to another with (FROM; TO)",
    ),
    scalar("td", "duration, short"),
    scalar(
        "calendar",
        "the instant or period read in a calendar with (NAME); bare, the name of its reading",
    ),
    scalar(
        "era",
        "the era of the instant's calendar reading, as its CLDR code",
    ),
    scalar(
        "strptime",
        "the instant read under a format, a locale and a calendar",
    ),
    scalar("tp", "strptime, short"),
    scalar(
        "strftime",
        "the instant written under a format and a locale",
    ),
    scalar("tfmt", "strftime, short"),
    with_null(
        scalar("timex", "the time expressions of the text"),
        OnNull::Nothing,
    ),
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
        "the topic through a shell command, read raw to the balancing paren: \
         sh(tr a-z A-Z) (needs --allow-shell)",
    ),
    // Reductions.
    with_options(
        reducing(
            "count",
            "how many; --values (-v) the non-null values alone, --nulls (-n) the nulls alone",
        ),
        &["--values", "-v", "--nulls", "-n"],
    ),
    reducing("sum", "the sum"),
    reducing("product", "the product"),
    reducing("min", "the least"),
    reducing("max", "the greatest"),
    reducing("mean", "the arithmetic mean"),
    reducing("describe", "the summary statistics: count, mean, std, min, the quartiles, max"),
    reducing("nulls", "the count of null fields; on a table one count per row"),
    FnSpec {
        options: &["--pretty", "--precision", "-p"],
        ..scalar("session", "the session's settings: --pretty=always|structures|off prints the results through pretty, --precision=n sets its digits; alone, the settings as a record")
    },
    scalar("stack", "the header's innermost level moved into the index"),
    scalar("unstack", "the index's innermost level moved into the header"),
    reducing("avg", "mean, alias"),
    reducing("median", "the median"),
    reducing("percentile", "the Nth percentile"),
    with_check(
        with_options(reducing(
            "std",
            "the standard deviation (sample with the word sample)",
        ), &["--sample"]),
        Check::Stat,
    ),
    with_check(
        with_options(reducing("var", "the variance (n − 1 with --sample)"), &["--sample"]),
        Check::Stat,
    ),
    with_check(with_options(reducing("stddev", "std, alias"), &["--sample"]), Check::Stat),
    with_check(with_options(reducing("variance", "var, alias"), &["--sample"]), Check::Stat),
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
        Check::Command,
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
        Check::Command,
    ),
    with_check(
        reducing("trigrams", "the adjacent triples of the stream (ngrams(3))"),
        Check::Command,
    ),
    with_check(
        with_options(
            reducing(
                "frequencies",
                "each distinct value with its count, most frequent first, as \
                 records (value, n); frequencies(K) keeps the top K; --nulls (-n) adds a \
                 row for the nulls, --values (-v) the default without it",
            ),
            &["--values", "-v", "--nulls", "-n"],
        ),
        Check::Command,
    ),
    with_check(
        reducing(
            "bins",
            "the numbers of the stream binned: n equal-width bins over min..max \
             (10 unless given) as the table #(@(from; to; count)), the nulls a last row",
        ),
        Check::Command,
    ),
    with_check(
        with_options(
            reducing(
                "histogram",
                "the stream drawn as bars, one text value: numbers in n equal-width \
                 bins (10 unless given), other values one bar per distinct value, most \
                 frequent first (n the most shown, the rest one 'other' bar), the nulls \
                 last; --width (-w) the longest bar in cells (40)",
            ),
            &["--width", "-w"],
        ),
        Check::Command,
    ),
    with_check(
        reducing("combinations", "every combination of N capsae"),
        Check::Command,
    ),
    with_check(reducing("pairs", "every pair of capsae"), Check::Command),
    with_check(
        reducing("triples", "every triple of capsae"),
        Check::Command,
    ),
    with_options(
        with_check(
            reducing(
                "sort",
                "the capsae sorted by value (sort(LOCALE), sort(atergo)); --desc (-d) descending",
            ),
            Check::Keyed,
        ),
        &["--desc", "-d"],
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
    // Whole-context commands.
    context("ungroup", "the groups' members as the context again"),
    with_check(
        context(
            "window",
            "each capsa with its neighbours: window(-2..0), window(3)",
        ),
        Check::Command,
    ),
    with_check(
        // after `|` the value form: a list's items, a record's or an
        // indexed table's values moved, the keys and labels kept
        FnSpec {
            scalar: true,
            ..context("shift", "each capsa's value from N capsae back")
        },
        Check::Command,
    ),
    with_check(
        with_options(context("zscore", "each value standardized over the context"), &["--sample"]),
        Check::Command,
    ),
    with_check(
        context("outline", "the context as a nested outline"),
        Check::Command,
    ),
    with_check(
        context("timeline", "the context ordered on its time anchors"),
        Check::Command,
    ),
    with_check(
        with_options(
            on_node(context(
                "tree",
                "the context drawn as a tree, the mount root on top, each node a starting point with its subtree and its crosslinks; operands: a depth and column expressions; switches --kind (-k), --traits (-t), --props (-p), --record (-r); --sample=n (-s=n) draws the first n children of every node that has more",
            )),
            &["--kind", "-k", "--traits", "-t", "--props", "-p", "--record", "-r", "--sample", "-s"],
        ),
        Check::Command,
    ),
    with_check(
        rendering(
            "csv",
            "the records as CSV (closing: the tool renders it); after a table value, the table as CSV",
        ),
        Check::Command,
    ),
    with_check(
        FnSpec {
            options: &["--precision", "-p"],
            ..rendering(
                "pretty",
                "the records as an aligned table (closing: the tool renders it); after a value, the value pretty-printed: a table the grid, a record one field per line, a list one item per line; a float with at most six digits after the point (-p=n sets them), numeric columns right-aligned",
            )
        },
        Check::Command,
    ),
    context(
        "encoding",
        "the output encoding (closing: the tool applies it)",
    ),
    // Keyed aggregates.
    with_options(
        keyed("sort_by", "the capsae sorted by keys; --desc (-d) descending"),
        &["--desc", "-d"],
    ),
    keyed("unique_by", "the first capsa per distinct key"),
    keyed("min_by", "the capsa with the least key"),
    keyed("max_by", "the capsa with the greatest key"),
    keyed("top", "the N capsae with the greatest key"),
    keyed("bottom", "the N capsae with the least key"),
    keyed("group", "the capsae grouped by keys"),
];

/// The registered command called `name`.
pub fn spec(name: &str) -> Option<&'static FnSpec> {
    SPECS.iter().find(|s| s.name == name)
}

/// The commands that may stand in a position, for enumeration
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

/// The environment prefix and command name of a call, when the call
/// is an environment command (`sh:tr` → `("sh", "tr")`).
pub fn split_env(name: &str) -> Option<(&str, &str)> {
    let (alias, cmd) = name.split_once(':')?;
    (!alias.is_empty() && !cmd.is_empty()).then_some((alias, cmd))
}

/// Whether `name` is a keyed aggregate (for `@|`).
/// The layer a known command answers from.
pub fn layer(name: &str) -> Option<Layer> {
    spec(name).map(|s| s.layer)
}

/// The names of the commands of one layer, in the manual's order.
pub fn names_of_layer(layer: Layer) -> impl Iterator<Item = &'static str> {
    SPECS
        .iter()
        .filter(move |s| s.layer == layer)
        .map(|s| s.name)
}

pub fn known_keyed(name: &str) -> bool {
    spec(name).is_some_and(|s| s.agg == Some(Agg::Keyed))
}

/// Whether `name` is a per-capsa scalar command (for `|`).
pub fn known_scalar(name: &str) -> bool {
    spec(name).is_some_and(|s| s.scalar)
}

/// Whether `name` is an aggregate command (for `@|`).
pub fn known_agg(name: &str) -> bool {
    spec(name).is_some_and(|s| s.agg.is_some())
}
