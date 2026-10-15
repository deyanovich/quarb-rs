# quarb-calc

**`qc`, the Quarb calculator, prebuilt** — no Rust toolchain, no
compile:

```sh
pip install quarb-calc
qc 3 + 5
qc '@(1; 2) +| 3'
qc '#(@(title; year); %("Hamlet"; 1603); %("Emma"; 1815)) | types'
```

`qc` is [`qua`](https://pypi.org/project/quarb-full/) with every
adapter off: the value grammar, the value space (scalars,
coercions, casts, lists, records, tables, record arrays, typed
columns), the standard library's value commands, the pipes over
streams of values, and subcontexts. It is the same source as
`qua`, built without the sources `qua` can open, never a second
implementation: the output is byte-identical.

`qc` runs what `qua` runs after the expression head `=`; the `=`
is accepted, so any calculator query pastes from a `qua`
transcript unchanged, but it is not required. The calculator
takes no input, so every argument is expression text, joined with
spaces: quotes are needed only around what the shell would take
for itself (`|`, `*`, `;`, parentheses, `$`, `"`). A query that
navigates — a path from a node, a mount — is refused by name,
pointing at `qua`.

The output forms are `qua`'s: the Quarb form by default,
`--json`, `--jsonl`, `--csv`, `--pretty`, `--save FILE` into
SQLite, `--kaiv`.

Part of [Quarb](https://quarb.org), a query language for
*arbors* — tree-spanned graphs. The full CLI is
[`quarb-full`](https://pypi.org/project/quarb-full/); the Python
bindings are [`quarb`](https://pypi.org/project/quarb/). See
[quarb.org](https://quarb.org) for the language specification,
the user guide, and the cookbooks.
