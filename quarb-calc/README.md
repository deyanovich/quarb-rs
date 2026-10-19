# qc — the Quarb calculator

`qc` is [`qua`](https://crates.io/crates/qua) with every adapter
off: the value grammar, the value space (scalars, coercions, casts,
lists, records, tables, record arrays, typed columns), the standard
library's value commands, the four pipes over streams of values,
and subcontexts. No mounts, no adapters, no text model; the Python
command libraries only by feature (`--features python-libs`).

It is the same source as `qua`, built without the sources `qua` can
open, never a second implementation: a value rule tested on `qc` is
the rule the full build runs, and the output is byte-identical.

    cargo install quarb-calc    # or: pip install quarb-calc
    qc 3 + 5
    qc '@(1; 2) +| 3'
    qc '= #(@(title; year); %("Hamlet"; 1603)) | types'

`qc` runs what `qua` runs after the expression head `=`; the `=` is
accepted, so any calculator query pastes from a `qua` transcript
unchanged, but it is not required. The calculator takes no input,
so every argument is expression text, joined with spaces: quotes
are needed only around what the shell would take for itself (`|`,
`*`, `;`, parentheses, `$`, `"`). A query that navigates — a path
from a node, a mount — is refused by name, pointing at `qua`.

A pipe is the input: `qc` reads it whole, as one text, the topic
of its single thread, and never parses it (`cat notes.md | qc '$_
| wc'`; a document on a pipe is `qua`'s). `-l` makes one thread
per line, the line as its topic (`seq 5 | qc -l '$_ * 2'`, `ls |
qc -l '@| count'`): sugar for `$_ | lines |` at the head. A query
that opens with a pipe pipes from the topic (`echo a b c | qc '|
words @| count'`). At a terminal the topic is null.

The output forms are `qua`'s: the Quarb form by default, `--json`,
`--jsonl`, `--csv`, `--pretty`, `--save FILE` into SQLite, `--kaiv`.
