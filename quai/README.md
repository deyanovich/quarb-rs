# quai

Interactive [Quarb][quarb] — a session REPL where every line becomes a
reusable query.

```bash
cargo install quai
quai data.json
```

`quai` opens a session over one or more sources and holds it. Each
line you run is labelled `&1`, `&2`, … — and every label is a
reusable *macro*: not the printed value, but the *query* that produced
it, ready to continue through the pipe. It is the notebook loop, cell
by cell, except a cell is a path into your data.

```
&1  /teams/*/members/*
    /teams/0/members/0  ⋮  (every member, across all teams, one path)
&2  &1 | [/langs/*:: = 'Go'] | /name::
    ada
    eu
&3  &1 @| count
    6
```

- `&N` re-runs line N; continue with a pipe (`&N | /key::`,
  `&N | [pred]`, `&N @| count`).
- `&N!` re-runs line N *live* (re-reads the source); `&N#` replays it
  *frozen*, as it was when it ran — over a moving source they diverge,
  and the difference is the drift.
- `def &x: … ;` adds a named fragment; `:history`, `:reset`, `:help`,
  `:quit` are the commands.
- `--daemon` backs the session with a resident arbor (materialize once,
  shared across runs); it composes with `--cache`.

`quai` reaches every source the engine mounts — JSON, YAML, TOML, CSV,
XML, HTML, Markdown, SQLite, kaiv, the filesystem, git, archives, XLSX,
and source code — and several at once mount under one root for
cross-source `<=>` joins.

Try it in the browser at [demo.quarb.org/quai][playground] (no
install), and read the [graph cookbook][cookbook] for the recipes.
`pip install quai` gives the same REPL in Python.

[quarb]: https://quarb.org/
[playground]: https://demo.quarb.org/quai/
[cookbook]: https://quarb.org/cookbooks/quai.html

## License

Licensed under either of Apache License, Version 2.0 or the MIT license
at your option.
