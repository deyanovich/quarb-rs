# qua

Structure-aware query CLI for the Quarb engine.

`qua` runs [Quarb](https://quarb.org) queries from the command
line over files and services — JSON, YAML, TOML, CSV, XML, HTML,
spreadsheets, archives, filesystems, git, SQL databases, graph
and document stores, mailboxes, and more — one query language for
all of them. Quarb is a query language for *arbors*: tree-spanned
graphs, a hierarchical backbone enriched with crosslink relations.

## Install

```sh
cargo install qua
```

## Usage

```sh
qua '//user[::age > 30]::name' data.json
qua '/row[::status = "open"]::id' tickets.csv
```

See [quarb.org](https://quarb.org) for the language specification,
the user guide, and per-tool cookbooks (XPath, jq, SQL, Cypher,
pandas, CSS).

## Command libraries

A command library adds commands under its own prefix, linked in
behind a feature; the default build carries the standard library
only. `morph:` wraps pymorphy3, the Russian morphological analyser,
through an embedded Python (`lemma`, `tag`, `pos`, `parse`,
`inflect`), and needs the package with its dictionaries at run time:

```sh
cargo install qua --features morph
pip install pymorphy3 pymorphy3-dicts-ru
qua '//token<word>:: | lower @| morph:lemma @| frequencies(10)' corpus:book.txt
```

`sp:` and `sklearn:` wrap scipy and scikit-learn the same way,
for the stream after `@|`: a list of records is a table whose numeric
fields are the columns and whose first text field is the row's label.
Significance tests, correlations with their p-values, a regression
line and hierarchical clustering under `sp:`; principal components,
k-means, tf-idf and a topic model under `sklearn:`:

```sh
cargo install qua --features scipy,sklearn
pip install scipy scikit-learn
qua '/measures/* | %(::year; ::mean) @| sp:spearmanr(--digits=3)' shelf.db
qua '/scores/* | %(::book; ::z) @| sklearn:kmeans(-k=3)' shelf.db
```

Every other name of those packages, and all of numpy (`np:`) and
pandas (`pd:`), is reached by reflection: the function or method is
looked up in the package when the gate first meets the name, its
synopsis read from its own signature, and a method is a function
whose first parameter is the receiver, which the pipe fills. A
dotted name walks the package: a submodule, an instance, a class
constructed on the way with the options its constructor declares,
a method, an attribute of the result. The stream of records
crosses as a frame with its first text field as the index, or as
the matrix of its numeric fields; a frame or a two-dimensional
array coming back is a table (`#(...)`, below), a one-dimensional
array a list, and anything else (a fitted model, a distribution) a
*library value*, kept as it is for the next command and printed as
the package prints it, so a chain constructs once. An operand
keeps the kind its expression yields: a subcontext of records is a
table read the same way, one of numbers a sample, one library value
the object itself. The input fills the first parameter named as
data (`X` in `cross_val_score(estimator; X; y)`, so the estimator
is the operand), a library value on the pipe that is an instance of
the class the path names is the receiver of its methods (a fitted
estimator before `.predict`), and a constructor that takes no data
is built from its options alone:

```sh
cargo install qua --features numpy,pandas,scipy,sklearn
qua '/measures/* | %(::book; ::mean; ::median) @| pd:describe | pd:round(2)' shelf.db
qua '/measures/* | %(::book; ::mean; ::median) @| pd:corr(--method=spearman) @| @' shelf.db
qua '/measures/* | ::mean @| np:percentile(90)' shelf.db
qua '/measures/* | ::mean @| sp:stats.shapiro' shelf.db
qua '/scores/* | %(::book; ::x; ::y) @| sklearn:cluster.KMeans.fit_predict(3; --random-state=0)' shelf.db
qua '/scores/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/scores/* | ::genre)) @| sklearn:naive_bayes.GaussianNB.predict((^/new/* | %(::book; ::x; ::y))) @| @' shelf.db
qua '/scores/* | %(::book; ::x; ::y) @| sklearn:model_selection.cross_val_score((1 | sklearn:naive_bayes.GaussianNB); (^/scores/* | ::genre); --cv=5) @| @' shelf.db
```

`| @` reads a table or a library value by rows (a frame as records,
its index the first column unless positional, an array as nested
lists) and `| %` by columns (a record of columns); the output forms
(`--json`, `--csv`, `--table`) export the rows themselves. The same
two readings flip plain values too (a list of records to a record
of lists and back), and three reshapes need no library:
`transpose` on a list of lists, `pivot(row; column; value)` long to
wide, `melt(id)` wide to long.

The table is a value kind of its own, written `#(...)`: a header
list of names standing first, then the rows, each a record
(`#(@(name; age); %("Braund"; 22); %("Allen"; 35))`); positional
rows without a header are an array whose columns are the positions
(`#(%(1; 2); %(3; 4))`), rows from full records infer the header
from their names, and a list anywhere but first is a syntax error,
since an array is not a list of lists: a table holds its rows to
one width, which a list never promises. An unnamed item of a row is
positional in header order, a named one matches the header by name,
a short row fills its tail with holes (`null`), a long one is
refused. `@| #` is the table
reading of a stream (records a frame, lists an array, values a
sample); `| @` (`rows`) yields each row as the table has it, a
record under a header or a list on the array face; `| %` (`columns`)
a record of lists keyed by the header or by position (`1`, `2`, …,
read with `:2`); `| [n]` a row and `| [a..b]` a slice; `| header`
the names (null on the array face) and `| dim` the shape; `:age` a
column. Brackets glued to a value expression index it: `v[2]` the
item, `v[2..]` the sublist, `t[2]` the row, `t[2..3]` the rows,
`t[2; "age"]` or `t[2; 1]` the cell (`| cell(2; "age")` on the pipe),
a range in either position a column slice, a row slice or a
sub-table; `row-to-header` and `header-to-row` move the first row
up and the header down. A table in a document carries the trait
`tabula` in every substrate (the text model's lowered table, an HTML
or XML `table` element), so `//*<tabula>` finds it and `| #` on the
node reads it as a table: the header where the format declares one
(a row of `th`, a `thead`, a TEI label row, the lemmas of the text
model's cells), the array face otherwise, a spanned cell repeated,
an empty cell null, a number read as one; nothing is parsed until
asked. `pretty` is the renderer: `@| pretty` over a stream of
records (as `@| table` was; `table` and `--table` stay accepted
through 0.37), `| pretty` after a table value for the aligned grid,
and on any value a pretty print, a record one field per line, a list
one item per line; `| csv` after a table value serializes it as
`| json` does. A table prints as its literal with the header and each row
on a line of its own, so the output pastes back as the constructor,
and it crosses into a library command as its rows, so the frame or
the matrix is made of it:

```sh
qua '= #(@(name; age); %("Braund"; 22); %("Allen"; 35)) | [2]'
qua '/measures/* | %(::book; ::mean; ::semi) @| # @| sklearn:cluster.KMeans.fit_predict(3)' shelf.db
qua '/measures/* | %(::book; ::mean; ::semi) @| # | header' shelf.db
qua '/measures/* | %(::book; ::mean; ::semi) @| # @| csv' shelf.db
```

A query naming a library that is not linked, or whose requirement
does not import, is refused with the way to meet it.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
