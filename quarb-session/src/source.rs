//! A source as a host hands it over in text or bytes — the browser
//! hosts, which hold no path — read as the native opener reads a
//! target: its format word, the options a mount writes after `?`,
//! and the composing layer every native mount gets (spec: Mounts;
//! Grafting). Wasm-safe: nothing here touches a filesystem.
//!
//! The native opener (quarb-open) does the same from a path, with
//! the whole fleet behind it; this is its counterpart for a host
//! that fetched the bytes itself, so that one notebook reads the
//! same in both (ruling 2026-11-12: a mount line's options reach
//! the browser's adapters as written; a `mount NAME: ?options`
//! line re-reads a source under them).

use crate::doc::{Doc, Dyn};
use anyhow::{Context, Result, bail};
use quarb::NodeId;
use quarb_text::TextModel;
use std::rc::Rc;

/// What a host knows of a source.
#[derive(Clone, Copy, Default)]
pub struct Source<'a> {
    /// The format word: a notation (`json`, `csv`, `lines`, …), a
    /// reading of one (`text-html`, `corpus-atd`, `lit-tei`), or a
    /// binary form (`sqlite`, `site-tar`, `xlsx`, `zip`, `tar`,
    /// `text-docx`, `corpus-odt`).
    pub format: &'a str,
    /// The source's text, for a text format.
    pub text: Option<&'a str>,
    /// The source's bytes, for a binary one.
    pub bytes: Option<&'a [u8]>,
    /// The mount options as a target writes them after `?`
    /// (`null=NA&decimal=comma`); empty for none.
    pub options: &'a str,
    /// The source's own URL, when it has one: the identity a
    /// document's links resolve against.
    pub url: Option<&'a str>,
    /// The files the mount's options name (`conllu=`, `sentences=`,
    /// `cast=`, `places=`, `desm=`), each by the option's value as
    /// written and its text: the host read them, the reader finds a
    /// file by the value.
    pub files: &'a [(&'a str, &'a str)],
}

/// The readings a format word may open with.
const READINGS: [&str; 4] = ["text", "corpus", "lit", "koine"];

/// The options every source takes, as [`OPTIONS`] lists them in a
/// refusal.
const OPTIONS: &str = "format=, delimiter=, decimal=, null=, autounits, autounit=, units=, \
                       slugs, sci, root=, base=; a corpus: or lit: reading takes conllu=, \
                       sentences=, cast=, places=, desm=, modernize=, notes= as well, lit: \
                       scheme= too";

/// The options of the corpus and literary readings (the native
/// opener's `corpus_level` and `lit_level` take the same): the
/// files they name are the host's to hand over ([`Source::files`]).
#[derive(Default)]
struct Reading<'a> {
    conllu: Option<&'a str>,
    sentences: Option<&'a str>,
    desm: Vec<&'a str>,
    cast: Option<&'a str>,
    places: Option<&'a str>,
    modernize: Option<&'a str>,
    notes: Option<&'a str>,
    scheme: Option<&'a str>,
    /// The first of them a query named, for the refusal on a source
    /// under another reading.
    named: Option<&'a str>,
}

/// A mount's options, read off its query.
#[derive(Default)]
struct Options<'a> {
    root: Option<&'a str>,
    suffixes: Option<quarb::autounits::Suffixes>,
    slugs: Option<String>,
    units: Vec<(String, String)>,
    sci: bool,
    format: Option<&'a str>,
    csv: quarb_csv::CsvOptions,
    /// The CSV option a query named, for the refusal on a source
    /// that is no CSV.
    csv_named: Option<&'a str>,
    base: Option<&'a str>,
    reading: Reading<'a>,
}

impl<'a> Options<'a> {
    fn read(query: &'a str) -> Result<Options<'a>> {
        let pairs = query
            .trim_start_matches('?')
            .split('&')
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let (suffixes, slugs, units, rest) =
            quarb::autounits::take_options_with_units(pairs).map_err(anyhow::Error::msg)?;
        let mut o = Options {
            suffixes,
            slugs,
            units,
            ..Options::default()
        };
        for pair in rest {
            match pair.split_once('=') {
                Some(("root", v)) => o.root = Some(v),
                None if pair == "sci" => o.sci = true,
                Some(("sci", v)) => bail!(
                    "sci={v}: sci takes no value (it reads the tables scientifically: a \
                     superscript digit after a number an exponent, K the kelvin)"
                ),
                Some(("format", v)) => o.format = Some(v),
                Some(("delimiter", label)) => {
                    o.csv = std::mem::take(&mut o.csv)
                        .with_delimiter_label(label)
                        .map_err(anyhow::Error::new)?;
                    o.csv_named = Some(pair);
                }
                Some(("decimal", label)) => {
                    o.csv = std::mem::take(&mut o.csv)
                        .with_decimal_label(label)
                        .map_err(anyhow::Error::new)?;
                    o.csv_named = Some(pair);
                }
                Some(("null", label)) => {
                    o.csv = std::mem::take(&mut o.csv)
                        .with_null_label(label)
                        .map_err(anyhow::Error::new)?;
                    o.csv_named = Some(pair);
                }
                Some(("base", v)) => o.base = Some(v),
                Some((
                    key @ ("conllu" | "sentences" | "desm" | "cast" | "places" | "modernize"
                    | "notes" | "scheme"),
                    v,
                )) => {
                    let r = &mut o.reading;
                    match key {
                        "conllu" => r.conllu = Some(v),
                        "sentences" => r.sentences = Some(v),
                        "desm" => r.desm.push(v),
                        "cast" => r.cast = Some(v),
                        "places" => r.places = Some(v),
                        "modernize" => r.modernize = Some(v),
                        "notes" => r.notes = Some(v),
                        _ => r.scheme = Some(v),
                    }
                    r.named.get_or_insert(pair);
                }
                // the native opener runs it under the shell gate
                Some(("annotate", _)) => bail!(
                    "{pair:?} runs a command over the text, and this host runs none: annotate \
                     the text beforehand and name the layer with conllu="
                ),
                // the text is a string already: the host decoded it
                Some(("encoding", _)) => bail!(
                    "{pair:?} has no place here: the host hands the source over as text, \
                     decoded already"
                ),
                _ => bail!("unknown option {pair:?} — a source here takes {OPTIONS}"),
            }
        }
        Ok(o)
    }
}

/// The format word under a `format=` option: the notation it names
/// in place of the word's own. A reading written before a notation
/// is kept (`text-html` under `format=md` is `text-md`), and so is
/// the bare `corpus`, `lit` or `koine`; the bare `text` is a plain
/// text file by its name, which `notes.txt?format=csv` reads as
/// CSV.
fn with_notation(format: &str, notation: &str) -> String {
    let notation = match notation {
        "txt" | "plain" => "text",
        other => other,
    };
    match format.split_once('-') {
        Some((reading, _)) if READINGS.contains(&reading) => format!("{reading}-{notation}"),
        _ if matches!(format, "corpus" | "lit" | "koine") => format!("{format}-{notation}"),
        _ => notation.to_string(),
    }
}

impl Doc {
    /// Read a source a host hands over: the format word, the mount
    /// options, the composing layer. A text format takes `text`, a
    /// binary one `bytes`.
    pub fn read(src: &Source) -> Result<Doc> {
        let opts = Options::read(src.options)?;
        let format = match opts.format {
            Some(n) => with_notation(src.format, n),
            None => src.format.to_string(),
        };
        let format = format.as_str();
        let is_csv = matches!(format, "csv" | "tsv");
        if let Some(named) = opts.csv_named
            && !is_csv
        {
            bail!("{named:?} is a CSV option, and this source is read as {format}");
        }
        let is_site = matches!(format, "site-tar" | "web");
        if let Some(base) = opts.base
            && !is_site
        {
            bail!("\"base={base}\" is a site's option, and this source is read as {format}");
        }
        let reading = match format.split_once('-') {
            Some((r @ ("corpus" | "lit"), notation)) => Some((r, notation)),
            None if matches!(format, "corpus" | "lit") => Some((format, "")),
            _ => None,
        };
        if let Some(named) = opts.reading.named
            && reading.is_none()
        {
            bail!(
                "{named:?} is an option of the corpus: and lit: readings, and this source is \
                 read as {format}"
            );
        }
        let mut doc = match (format, src.bytes, src.text) {
            // a reading under its options: the files they name were
            // handed over beside the source
            _ if reading.is_some() && opts.reading.named.is_some() => {
                let (level, notation) = reading.unwrap_or_default();
                Doc::Text(reading_with(level, notation, src, &opts.reading)?)
            }
            ("sqlite" | "db", Some(bytes), _) => Doc::sqlite_bytes(bytes)?,
            // a workbook, every sheet a table: calamine tells xlsx,
            // xls and ods by the bytes
            #[cfg(feature = "xlsx")]
            ("xlsx" | "xls" | "ods", Some(bytes), _) => {
                let a = Rc::new(
                    quarb_xlsx::XlsxAdapter::from_bytes(bytes)
                        .map_err(|e| anyhow::anyhow!("reading the workbook: {e}"))?,
                );
                let r = a.clone();
                Doc::Boxed(
                    Dyn(Box::new(quarb_mount::Shared(a))),
                    Box::new(move |n| r.locator(n)),
                    "xlsx",
                )
            }
            ("xlsx" | "xls" | "ods", _, _) => binary_source(format, "a workbook", "xlsx")?,
            // an archive, a zip or a tar (plain or compressed) by its
            // first bytes
            #[cfg(feature = "archives")]
            ("zip" | "tar" | "archive", Some(bytes), _) => {
                let archive = quarb_archive::ArchiveAdapter::from_bytes(bytes)
                    .map_err(|e| anyhow::anyhow!("reading the archive: {e}"))?;
                // the whole composing layer, as the opener mounts an
                // archive: a member that names a notation grafts on
                // entry, and an archive inside it opens from its
                // bytes and composes in turn
                let a = Rc::new(
                    quarb_compose::ComposeAdapter::new(archive)
                        .with_source_bytes(quarb_compose::archive_member_bytes)
                        .with_string_reader(string_reader()),
                );
                let r = a.clone();
                Doc::Boxed(
                    Dyn(Box::new(quarb_mount::Shared(a))),
                    Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
                    "archive",
                )
            }
            ("zip" | "tar" | "archive", _, _) => binary_source(format, "an archive", "archives")?,
            // a word processor's file at the text level, and its
            // prose as a corpus
            ("text-docx" | "text-odt", _, _) => Doc::Text(document_text(&format[5..], src.bytes)?),
            ("corpus-docx" | "corpus-odt", _, _) => {
                let mut m = document_text(&format[7..], src.bytes)?;
                m.tokenize();
                Doc::Text(m)
            }
            ("lit-docx" | "lit-odt" | "koine-docx" | "koine-odt", _, _) => {
                let (level, kind) = format.split_once('-').unwrap_or_default();
                return Err(no_such_reading(level, kind));
            }
            ("sqlite" | "db", None, _) => bail!("a database is handed over as bytes"),
            // the base is the option's alone, as `web:site.tgz` is
            // read natively: the pages link by the paths they sit at
            ("site-tar" | "web", Some(bytes), _) => site_tarball(bytes, opts.base.unwrap_or(""))?,
            ("site-tar" | "web", None, _) => bail!("a site archive is handed over as bytes"),
            ("csv" | "tsv", _, Some(text)) => {
                let by_extension = if format == "tsv" { b'\t' } else { b',' };
                quarb_csv::CsvAdapter::parse_declared(text, by_extension, opts.csv.clone())
                    .map(Doc::Csv)
                    .with_context(|| format!("parsing {}", format.to_uppercase()))?
            }
            (_, _, Some(text)) => Doc::parse(text, format)?,
            (_, Some(_), None) => bail!("{format} is a text format: hand its text over"),
            (_, None, None) => bail!("a source needs text or bytes"),
        };
        if let Some(url) = src.url {
            doc.attach_url(url);
        }
        // the layers a native mount gets, in its order: the
        // composing layer, the scientific reading, the unit
        // convention, the root
        let family = doc.family();
        // around a database the whole layer, as the native opener
        // mounts one: a text column that holds JSON walks as a
        // subtree (an archive came with its own, above)
        let relational = family == "sqlite";
        let (adapter, render) = doc.into_mounted()?;
        let (adapter, render) = composed(adapter, render, relational);
        let (adapter, render): (Box<dyn quarb::AstAdapter>, Render) = if opts.sci {
            (Box::new(quarb::Scientific { inner: adapter }), render)
        } else {
            (adapter, render)
        };
        let (adapter, render): (Box<dyn quarb::AstAdapter>, Render) =
            if opts.suffixes.is_none() && opts.slugs.is_none() && opts.units.is_empty() {
                (adapter, render)
            } else {
                // `?units=pounds:_lb` alone reads the titles under
                // the override, as `?autounits` does
                let suffixes = opts.suffixes.or_else(|| {
                    (!opts.units.is_empty()).then_some(quarb::autounits::Suffixes::Known)
                });
                let wrapped = Rc::new(
                    quarb::autounits::AutoUnits::with(
                        adapter,
                        suffixes,
                        opts.slugs.clone(),
                        src.url.unwrap_or(""),
                    )
                    .with_units(opts.units.clone()),
                );
                let w = wrapped.clone();
                (
                    Box::new(quarb_mount::Shared(wrapped)),
                    Box::new(move |n| w.locate(n, &*render)),
                )
            };
        let (adapter, render): (Box<dyn quarb::AstAdapter>, Render) = match opts.root {
            None => (adapter, render),
            Some(root) => {
                let rooted = quarb::rooted::Rooted::at(adapter, root)
                    .map_err(|e| anyhow::anyhow!("root={root}: {e}"))?;
                // the locator renders relative to the root as the
                // paths do
                let root_loc = render(quarb::AstAdapter::root(&rooted));
                (
                    Box::new(rooted),
                    Box::new(move |n| {
                        let loc = render(n);
                        match loc.strip_prefix(root_loc.as_str()) {
                            Some("") => "/".to_string(),
                            Some(rest) if rest.starts_with('/') => rest.to_string(),
                            _ => loc,
                        }
                    }),
                )
            }
        };
        Ok(Doc::Boxed(Dyn(adapter), render, family))
    }
}

type Render = Box<dyn Fn(NodeId) -> String>;

/// The refusal for a binary source this build has no reader for, or
/// one handed over as text.
fn binary_source(format: &str, what: &str, feature: &str) -> Result<Doc> {
    let built = match feature {
        "xlsx" => cfg!(feature = "xlsx"),
        _ => cfg!(feature = "archives"),
    };
    if built {
        bail!("{what} is handed over as bytes")
    }
    bail!("this build has no reader for {format} ({what})")
}

/// The refusal of the literary or the koine reading over a word
/// processor's file: it declares no literary vocabulary, and this
/// host reads it at the text level alone.
fn no_such_reading(level: &str, kind: &str) -> anyhow::Error {
    let what = if kind == "docx" {
        "a Word document"
    } else {
        "an OpenDocument text"
    };
    if level == "lit" {
        anyhow::anyhow!(
            "lit: reads TEI, USX, OSIS, USFM, FictionBook, RNC, OpenCorpora, PROIEL and \
             litogramma — this source is {what}, whose prose corpus: reads (text: its \
             structure)"
        )
    } else {
        anyhow::anyhow!(
            "{level}: this host reads {what} at the text level alone — text: reads its \
             structure, corpus: its prose"
        )
    }
}

/// The text level of a word processor's file, from its bytes: a Word
/// document (`docx`) or an OpenDocument text (`odt`).
fn document_text(kind: &str, bytes: Option<&[u8]>) -> Result<TextModel> {
    let what = if kind == "docx" {
        "a Word document"
    } else {
        "an OpenDocument text"
    };
    let Some(bytes) = bytes else {
        bail!("{what} is handed over as bytes")
    };
    match kind {
        #[cfg(feature = "docx")]
        "docx" => quarb_text_docx::parse(bytes)
            .map_err(|e| anyhow::anyhow!("reading the Word document: {e}")),
        #[cfg(feature = "odt")]
        "odt" => quarb_text_odt::parse(bytes)
            .map_err(|e| anyhow::anyhow!("reading the OpenDocument text: {e}")),
        _ => {
            let _ = bytes;
            bail!("this build has no reader for {what}")
        }
    }
}

/// The composing layer around a source (spec: Grafting): grafting on
/// demand only (`graft(NOTATION)`, a `<NOTATION>` trait at a hop),
/// the leaves as they were; around a relational source the whole
/// layer, so a text column that holds JSON walks as a subtree, as
/// the native opener mounts one.
#[cfg(feature = "compose")]
fn composed(
    adapter: Box<dyn quarb::AstAdapter>,
    render: Render,
    relational: bool,
) -> (Box<dyn quarb::AstAdapter>, Render) {
    use quarb_compose::ComposeAdapter;
    if quarb::AstAdapter::grafts_on_demand(&adapter) {
        return (adapter, render);
    }
    let layer = ComposeAdapter::new(Dyn(adapter)).with_string_reader(string_reader());
    let layer = if relational {
        layer
    } else {
        layer.on_demand_only()
    };
    let layer = Rc::new(layer);
    let l = layer.clone();
    (
        Box::new(quarb_mount::Shared(layer)),
        Box::new(move |n| l.locator(n, |o| render(o))),
    )
}

#[cfg(not(feature = "compose"))]
fn composed(
    adapter: Box<dyn quarb::AstAdapter>,
    render: Render,
    _relational: bool,
) -> (Box<dyn quarb::AstAdapter>, Render) {
    (adapter, render)
}

/// The readings of a lone string, for `graft(READING:NOTATION)`:
/// the same readers a source's format word names.
#[cfg(feature = "compose")]
fn string_reader() -> quarb_compose::StringReader {
    Rc::new(|reading: &str, notation: &str, text: &str| {
        READINGS
            .contains(&reading)
            .then(|| text_reading(reading, notation, text).map_err(|e| format!("{e:#}")))
    })
}

/// A site's pages from a tarball (gzipped or not), the web level
/// over them: every page grafted at the text level on entry.
fn site_tarball(bytes: &[u8], base: &str) -> Result<Doc> {
    let site = quarb_web::archive::open_tar(bytes, base)
        .map_err(|e| anyhow::anyhow!("reading the archive as a site: {e}"))?;
    let a = Rc::new(site);
    let r = a.clone();
    Ok(Doc::Boxed(
        Dyn(Box::new(quarb_mount::Shared(a))),
        Box::new(move |n| r.locator(n)),
        "web",
    ))
}

/// The corpus or literary reading of a text under its options: the
/// native opener's `corpus_level` and `lit_level` with
/// `finish_corpus`, the files read from what the host handed over
/// instead of a disk. Keep the steps and their order in step with
/// those three: the notes and the spelling table first, then the
/// tier (a given segmentation, an annotation layer, or the tokenizer
/// under the mount's bonds), then the cast and places tables.
fn reading_with(level: &str, notation: &str, src: &Source, o: &Reading) -> Result<TextModel> {
    let files = src.files;
    let text = || -> Result<&str> {
        src.text
            .ok_or_else(|| anyhow::anyhow!("{level}: this source is a text: hand its text over"))
    };
    let file = |option: &str, name: &str| -> Result<&str> {
        files
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{level}: {option}={name} names a file this host was not handed (a \
                     notebook's mount names it by URL; a source without one has no second file)"
                )
            })
    };
    let table = |option: &str, name: Option<&str>| -> Result<Option<Vec<quarb_text::CastRow>>> {
        let Some(name) = name else { return Ok(None) };
        let what = if option == "cast" { "cast" } else { "places" };
        quarb_text::CastRow::read_csv_text(file(option, name)?)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{level}: reading the {what} table {name}: {e}"))
    };
    let notation = notation.to_ascii_lowercase();
    let notation = notation.as_str();
    anyhow::ensure!(
        o.scheme.is_none() || level == "lit",
        "{level}: scheme= is the literary reading's option (lit:)"
    );
    let mut model = match (level, notation) {
        // A treebank is its own annotation and its own tokens: the
        // spelling table and the two tables still apply.
        ("corpus", "conllu" | "conllup") => {
            anyhow::ensure!(
                o.conllu.is_none(),
                "corpus: a treebank is its own annotation — conllu= applies to prose"
            );
            let mut m = TextModel::parse_conllu_corpus(text()?)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?;
            if let Some(t) = o.modernize {
                m.set_orthography(t)
                    .map_err(|e| anyhow::anyhow!("corpus: {e}"))?;
            }
            if let Some(rows) = table("cast", o.cast)? {
                m.apply_cast(&rows);
            }
            if let Some(rows) = table("places", o.places)? {
                m.apply_places(&rows);
            }
            return Ok(m);
        }
        ("corpus", "docx" | "odt") => document_text(notation, src.bytes)?,
        ("corpus", _) => text_level(notation, text()?)?,
        (_, "docx" | "odt") => return Err(no_such_reading(level, notation)),
        _ => lit_level(notation, text()?, o.scheme)?,
    };
    match o.notes {
        None => {}
        Some("omit") => model.omit_notes(true),
        Some(v) => {
            bail!("{level}: notes={v} — notes= takes omit (a note is part of the text by default)")
        }
    }
    if let Some(t) = o.modernize {
        model
            .set_orthography(t)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?;
    }
    if let Some(name) = o.sentences {
        anyhow::ensure!(
            o.conllu.is_none(),
            "{level}: sentences= is a given segmentation of its own; it takes neither \
             conllu= nor annotate="
        );
        anyhow::ensure!(
            o.desm.is_empty(),
            "{level}: sentences= gives the sentences as they are; desm= does not apply"
        );
        model
            .tokenize_given(file("sentences", name)?)
            .map_err(|e| anyhow::anyhow!("{level}: {name}: {e}"))?;
    } else if let Some(name) = o.conllu {
        model
            .annotate_conllu(file("conllu", name)?)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?;
    } else {
        // this mount's own bonds; a bonds file that draws on another
        // has only its own text here
        let mut bonds: Option<syndesmos::Syndesmos> = None;
        for name in &o.desm {
            let more = syndesmos::Syndesmos::parse(file("desm", name)?)
                .map_err(|e| anyhow::anyhow!("reading {name}: {e}"))?;
            bonds
                .get_or_insert_with(syndesmos::Syndesmos::empty)
                .extend(more);
        }
        model.tokenize_with(bonds.as_ref());
    }
    if let Some(rows) = table("cast", o.cast)? {
        model.apply_cast(&rows);
    }
    if let Some(rows) = table("places", o.places)? {
        model.apply_places(&rows);
    }
    Ok(model)
}

/// A text under a reading, by its notation: what `text:`, `corpus:`,
/// `lit:` and `koine:` read from a file of that extension. The
/// readings' own options (`conllu=`, `cast=`, …) go through
/// [`Doc::read`], which holds the files they name.
pub(crate) fn text_reading(reading: &str, notation: &str, text: &str) -> Result<TextModel> {
    let notation = notation.to_ascii_lowercase();
    let notation = notation.as_str();
    match reading {
        "text" => text_level(notation, text),
        "corpus" => {
            let mut m = match notation {
                // a treebank is its own tokens
                "conllu" | "conllup" => {
                    return TextModel::parse_conllu_corpus(text)
                        .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"));
                }
                _ => text_level(notation, text)?,
            };
            m.tokenize();
            Ok(m)
        }
        "lit" => {
            let mut m = lit_level(notation, text, None)?;
            m.tokenize();
            Ok(m)
        }
        "koine" => koine_level(notation, text),
        other => bail!("'{other}' is no reading (text, corpus, lit, koine)"),
    }
}

/// The text level of a text, by its notation: the native opener's
/// dispatch by extension.
fn text_level(notation: &str, text: &str) -> Result<TextModel> {
    Ok(match notation {
        // A treebank read as a document: sections, paragraphs and
        // sentences from its comments.
        "conllu" | "conllup" => TextModel::parse_conllu_text(text)
            .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?,
        "html" | "htm" => quarb_text_html::parse(text),
        "md" | "markdown" => quarb_text_markdown::parse(text),
        "tex" | "latex" => quarb_text_latex::parse(text),
        "txt" | "text" | "plain" => TextModel::parse_plain(text),
        "rtf" => quarb_text_rtf::parse(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("reading RTF: {e}"))?,
        // the editions and the XML vocabularies, through the koine
        // reader
        "atd" | "atk" | "usfm" | "sfm" | "fb2" | "xml" | "tei" => koine_edition(notation, text)?,
        _ if text.trim_start().starts_with('<') && xml_kind(text).is_some() => {
            koine_edition("xml", text)?
        }
        // a DOCTYPE declares markup; a bare `<` declares nothing
        _ if declares_html(text) => quarb_text_html::parse(text),
        _ => TextModel::parse_plain(text),
    })
}

/// Whether a text opens with an HTML DOCTYPE.
fn declares_html(text: &str) -> bool {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    // by `get`: the ninth byte of a text that opens with a curly
    // quote is no character boundary
    t.get(..9)
        .is_some_and(|w| w.eq_ignore_ascii_case("<!doctype"))
        && t[9..]
            .trim_start()
            .get(..4)
            .is_some_and(|w| w.eq_ignore_ascii_case("html"))
}

#[cfg(feature = "koine")]
fn xml_kind(text: &str) -> Option<&'static str> {
    quarb_text_koine::detect_xml_kind(text)
}

#[cfg(not(feature = "koine"))]
fn xml_kind(_text: &str) -> Option<&'static str> {
    None
}

/// An edition or an XML vocabulary at the text level, through the
/// koine reader: an atrep document, USFM, FictionBook, or an XML
/// file by its declared identity.
#[cfg(feature = "koine")]
fn koine_edition(notation: &str, text: &str) -> Result<TextModel> {
    use std::path::Path;
    match notation {
        "atd" | "atk" => quarb_text_koine::parse_str(strip_bom(text), Path::new(""))
            .map_err(|e| anyhow::anyhow!("reading the atrep document: {e}")),
        "usfm" | "sfm" => quarb_text_koine::parse_xml_as(text, "usfm")
            .map_err(|e| anyhow::anyhow!("reading USFM: {e}")),
        "fb2" => quarb_text_koine::parse_xml_as(text, "fb2")
            .map_err(|e| anyhow::anyhow!("reading FictionBook: {e}")),
        _ => match quarb_text_koine::detect_xml_kind(text) {
            Some(kind) => quarb_text_koine::parse_xml_as(text, kind)
                .map_err(|e| anyhow::anyhow!("reading {kind} through the koine route: {e}")),
            None => bail!(
                "the file declares no XML identity the text model knows (no namespace, \
                 DOCTYPE, or unambiguous root) — name the vocabulary with \
                 ?format=tei|docbook|jats|usx|osis|fb2|rnc|opencorpora|proiel under koine:, \
                 or read the markup itself as xml"
            ),
        },
    }
}

#[cfg(not(feature = "koine"))]
fn koine_edition(notation: &str, _text: &str) -> Result<TextModel> {
    bail!("this build has no reader for {notation} at the text level")
}

#[cfg(feature = "koine")]
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// The literary reading: an atrep document or one of the literary
/// XML vocabularies, every inline simmere kept as a node.
#[cfg(feature = "koine")]
fn lit_level(notation: &str, text: &str, scheme: Option<&str>) -> Result<TextModel> {
    use std::path::Path;
    let kind = match notation {
        "atd" | "atk" => "atd",
        "usx" => "usx",
        "usfm" | "sfm" => "usfm",
        "fb2" => "fb2",
        "tei" | "osis" | "rnc" | "opencorpora" | "proiel" => notation,
        _ => match quarb_text_koine::detect_xml_kind(text) {
            Some(k) => k,
            None => bail!(
                "lit: reads TEI, USX, OSIS, FictionBook, RNC, OpenCorpora, PROIEL and \
                 litogramma — this text declares none of them; the corpus reading reads prose"
            ),
        },
    };
    match kind {
        "atd" => {
            anyhow::ensure!(
                scheme.is_none(),
                "lit: scheme= over an atrep document needs the file beside its includes; \
                 this host holds one text"
            );
            quarb_text_koine::parse_lit_str(strip_bom(text), Path::new(""))
                .map_err(|e| anyhow::anyhow!("reading the atrep document: {e}"))
        }
        "tei" | "usx" | "osis" | "fb2" | "usfm" | "rnc" | "opencorpora" | "proiel" => {
            quarb_text_koine::parse_lit_xml_as_with(text, kind, scheme)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", kind.to_uppercase()))
        }
        other => bail!(
            "lit: reads TEI, USX, OSIS, USFM, RNC, OpenCorpora, PROIEL and litogramma — not \
             {other} (the koine reading reads the others)"
        ),
    }
}

#[cfg(not(feature = "koine"))]
fn lit_level(notation: &str, _text: &str, _scheme: Option<&str>) -> Result<TextModel> {
    bail!("this build has no literary reading of {notation}")
}

/// The koine route: atrep's importer fed the text, by its notation.
#[cfg(feature = "koine")]
fn koine_level(notation: &str, text: &str) -> Result<TextModel> {
    use std::path::Path;
    let text = strip_bom(text);
    let imported = match notation {
        "atd" | "atk" => quarb_text_koine::parse_str(text, Path::new("")),
        "usfm" | "sfm" => quarb_text_koine::parse_xml_as(text, "usfm"),
        "md" | "markdown" => quarb_text_koine::parse_markdown(text),
        "html" | "htm" => quarb_text_koine::parse_html(text),
        "rst" => quarb_text_koine::parse_rst(text),
        "org" => quarb_text_koine::parse_org(text),
        "dj" | "djot" => quarb_text_koine::parse_djot(text),
        "tei" | "docbook" | "jats" | "usx" | "osis" | "fb2" | "rnc" | "opencorpora" | "proiel" => {
            quarb_text_koine::parse_xml_as(text, notation)
        }
        "xml" => match quarb_text_koine::detect_xml_kind(text) {
            Some(kind) => quarb_text_koine::parse_xml_as(text, kind),
            None => bail!(
                "the file declares no XML identity this route knows (no namespace, DOCTYPE, \
                 or unambiguous root) — force one with ?format=tei|docbook|jats|usx|osis|fb2"
            ),
        },
        other => bail!("no atrep import for '{other}' — the text reading reads it"),
    };
    imported.map_err(|e| anyhow::anyhow!("importing through atrep: {e}"))
}

#[cfg(not(feature = "koine"))]
fn koine_level(notation: &str, _text: &str) -> Result<TextModel> {
    bail!("this build has no koine reading of {notation}")
}
