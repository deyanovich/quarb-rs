//! The honest structural subset over hand-written LaTeX: declared
//! sectioning, the at-callout footnote convention normalized to
//! document-end bodies, explicit-number footnotemark/footnotetext
//! pairing, quotes, lists, verbatim, comments, and the
//! unwrap-vs-drop prose rules.

use quarb::QueryResult;
use quarb_text_latex::parse;

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

const DOC: &str = r#"\documentclass{article}
\usepackage{hyperref}
\title{The Emu War}

\begin{document}
\section{The war}\label{sec:war}

Emus \emph{advanced} on the wheat.\footnote{Twenty thousand of
them.} % a comment
The gunners followed.

\subsection{First attempt}

The \texttt{Lewis gun} jammed.\footnote{At Campion, November
1932.}

\begin{quote}
They can face machine guns.
\end{quote}

\begin{itemize}
\item alpha
\item beta
\end{itemize}

\begin{verbatim}
qua '//section' war.tex
\end{verbatim}

See section~\ref{sec:war}.
\end{document}
"#;

#[test]
fn declared_sections_nest() {
    let m = parse(DOC);
    assert_eq!(values(&m, "/section::lemma"), ["The war"]);
    assert_eq!(values(&m, "/section/section::lemma"), ["First attempt"]);
}

#[test]
fn prose_reads_clean() {
    let m = parse(DOC);
    // \emph unwraps, the comment vanishes, the callout leaves no
    // marker, the soft-wrapped sentence joins.
    assert_eq!(
        values(&m, r#"//section[::lemma = "The war"]/paragraph[1]::"#),
        ["Emus advanced on the wheat. The gunners followed."]
    );
    // \ref keeps its key as written; \label vanished.
    assert_eq!(
        values(&m, r#"//paragraph[:: *= "sec:war"]::"#),
        ["See section sec:war."]
    );
}

#[test]
fn footnotes_are_the_apparatus() {
    let m = parse(DOC);
    // two callouts, two bodies — //footnote gathers all four
    assert_eq!(values(&m, "//footnote @| count"), ["4"]);
    // the callout under its paragraph resolves to its body
    assert_eq!(
        values(
            &m,
            r#"//section[::lemma = "The war"]/paragraph/footnote->footnote::"#
        ),
        ["Twenty thousand of them."]
    );
    // print order names the onyms
    assert_eq!(values(&m, "//*<note>::onym"), ["1", "2"]);
    // which paragraph cites note 2 — the citation walk
    assert_eq!(
        values(&m, r#"//*<note>[::onym = "2"]<-footnote\*::"#),
        ["The Lewis gun jammed."]
    );
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
}

#[test]
fn quote_list_verbatim() {
    let m = parse(DOC);
    assert_eq!(values(&m, "//blockquote::"), ["They can face machine guns."]);
    assert_eq!(values(&m, "//unordered-item::"), ["alpha", "beta"]);
    assert_eq!(values(&m, "//verbatim::"), ["qua '//section' war.tex"]);
}

#[test]
fn footnotemark_pairs_by_number() {
    let m = parse(
        r#"\section{S}
One.\footnotemark[7] Two.

\footnotetext[7]{The seventh.}"#,
    );
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["The seventh."]
    );
    assert_eq!(values(&m, "//*<note>::onym"), ["7"]);
}

#[test]
fn dangling_mark_lints() {
    let m = parse(r#"\section{S} Text.\footnotemark[9]"#);
    assert_eq!(values(&m, "//*<dangling>::onym"), ["9"]);
    assert_eq!(values(&m, "//*<dangling>::::resolved"), ["false"]);
    // the dangling callout projects its raw onym
    assert_eq!(values(&m, "//*<dangling>::"), ["9"]);
}

#[test]
fn endnotes_are_their_own_family() {
    let m = parse(
        r#"\section{S}
The plan.\endnote{Filed with the ministry.} The field.\footnote{At Campion.}

\theendnotes"#,
    );
    // one callout + one body per family
    assert_eq!(values(&m, "//endnote @| count"), ["2"]);
    assert_eq!(values(&m, "//footnote @| count"), ["2"]);
    // the edge carries the family name
    assert_eq!(
        values(&m, "//*<deixis>->endnote::"),
        ["Filed with the ministry."]
    );
    // families keep separate counters: both onyms are "1"
    assert_eq!(values(&m, "//*<note>::onym"), ["1", "1"]);
    // <note> marks the bodies — one per family here
    assert_eq!(values(&m, "//*<note> @| count"), ["2"]);
}

#[test]
fn index_marks_are_declared_anchors() {
    let m = parse(
        r#"\section{The war}
Emus\index{emu} advanced.\index{wheat districts|textbf}

More prose.\index{gun!Lewis}"#,
    );
    // marks in flow position: terms as written, `|...` directives
    // stripped, `!` subentries kept
    assert_eq!(
        values(&m, "//index-mark::term"),
        ["emu", "wheat districts", "gun!Lewis"]
    );
    // invisible in the surrounding prose
    assert_eq!(values(&m, "/section/paragraph[1]::"), ["Emus advanced."]);
}

#[test]
fn nested_lists_close_their_own_items() {
    // A nested enumerate inside an itemize item must not eat the
    // outer item's close — the section AFTER the lists proves the
    // container stack fully unwound (the spec.tex regression).
    let m = parse(
        r#"\section{A}
\begin{itemize}
\item outer
\begin{enumerate}
\item inner
\end{enumerate}
\item outer two
\end{itemize}

\section{B}

After."#,
    );
    assert_eq!(values(&m, "//section::lemma"), ["A", "B"]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        ["outer\ninner", "outer two"]
    );
    assert_eq!(values(&m, "//ordered-item::"), ["inner"]);
    assert_eq!(values(&m, r#"//section[::lemma = "B"]/paragraph::"#), ["After."]);
}

#[test]
fn verb_is_inline_verbatim() {
    // \verb's payload is copied as authored — a $, %, or brace
    // inside must not derail the scan (the spec.tex regression:
    // \verb|$_| opened a math scan that ate the next heading).
    let m = parse(
        r#"\section{A}
The topic is \verb|$_| in Perl style. Also \verb!50%! and
\verb+a{b+ survive.

\section{B}

After."#,
    );
    assert_eq!(values(&m, "//section::lemma"), ["A", "B"]);
    assert_eq!(
        values(&m, "/section[1]/paragraph::"),
        ["The topic is $_ in Perl style. Also 50% and a{b survive."]
    );
}

#[test]
fn escaped_dollar_inside_math_does_not_close_it() {
    // $\$*1, \$*2$ — the \$ inside math must not end the math
    // scan (the spec.tex regression: parity flipped and the next
    // heading was eaten).
    let m = parse(
        r#"\section{A}
Trace references $\$*1, \$*2$ name expressions.

\section{B}

After."#,
    );
    assert_eq!(values(&m, "//section::lemma"), ["A", "B"]);
}

#[test]
fn sidenotes_are_footnotes_in_the_margin() {
    // Placement is presentation: \sidenote joins the footnote
    // family (shared counter and namespace); the declared
    // spelling survives as ::::form = "margin" on both ends.
    let m = parse(
        r#"\section{S}
The war began.\footnote{In October.} It went
badly.\sidenote{By November it was over.}"#,
    );
    assert_eq!(values(&m, "//footnote @| count"), ["4"]);
    assert_eq!(values(&m, "//*<note>::onym"), ["1", "2"]);
    assert_eq!(
        values(&m, r#"//*<note>[::::form = "margin"]::"#),
        ["By November it was over."]
    );
    assert_eq!(
        values(&m, r#"//*<deixis>[::::form = "margin"]->footnote::"#),
        ["By November it was over."]
    );
    // the plain footnote carries no form
    assert_eq!(values(&m, "//*<note>[::::form] @| count"), ["1"]);
}

#[test]
fn margin_content_is_the_aside_family() {
    // \marginpar and \marginnote are unnumbered anchored content
    // — litogramma's aside: deixis at the flow point, body at
    // the document end, ->aside edge, and NO <note> (content,
    // not apparatus).
    let m = parse(
        r#"\section{S}
The advance stalled.\marginpar{See the map.}

A second push.\marginnote{Contested figure.}[2mm]"#,
    );
    assert_eq!(values(&m, "//aside @| count"), ["4"]);
    assert_eq!(
        values(&m, "//*<deixis>->aside::"),
        ["See the map.", "Contested figure."]
    );
    // which paragraph does the first aside annotate — bodies
    // sit at the document root, anchors in the flow
    assert_eq!(
        values(&m, r#"/aside[::onym = "1"]<-aside\*::"#),
        ["The advance stalled."]
    );
    assert_eq!(values(&m, "//*<note> @| count"), ["0"]);
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
}

#[test]
fn verse_environment_lowers_to_the_verse_vocabulary() {
    let m = parse(
        r#"\section{Ode on Solitude}
\begin{verse}
Happy the man, whose wish and care \\
A few \emph{paternal} acres bound,

Content to breathe his native air, \\
In his own ground.
\end{verse}"#,
    );
    assert_eq!(values(&m, "//verse @| count"), ["1"]);
    assert_eq!(values(&m, "//strophe @| count"), ["2"]);
    assert_eq!(
        values(&m, "//stichos[::taxis = 2]::"),
        ["A few paternal acres bound,"]
    );
    assert_eq!(values(&m, "//stichos[::taxis = 4]::"), ["In his own ground."]);
}

#[test]
fn multibyte_prose_survives() {
    // The scanner pushed bytes as chars, Latin-1-izing UTF-8 —
    // curly quotes and Greek in free prose mangled (found by the
    // Iliad fixtures: "unnumber’d", χραισμεῖν).
    let m = parse(
        "\\section{S}\nOf χραισμεῖν, Buttmann observes “it helps thee not”.",
    );
    assert_eq!(
        values(&m, "/section/paragraph::"),
        ["Of χραισμεῖν, Buttmann observes “it helps thee not”."]
    );
}
