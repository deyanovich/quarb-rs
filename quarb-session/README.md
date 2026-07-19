# quarb-session

The backend-agnostic interactive [Quarb][quarb] session — the library
behind the [`quai`][quai] REPL.

A `Session` holds a materialized source and a growing macro table where
each accepted line becomes `def &N: <line> ;`: history is the
language's own reuse mechanism, not a bolted-on cell store. It sits on
two pluggable seams so the same logic serves the terminal, a resident
daemon, and the browser:

- an **`Executor`** — where the arbor materializes and queries run
  (`LocalExecutor` in-process, or a daemon executor over a socket);
- a **`Store`** — where the session's durable state persists
  (`FileStore`, or `MemStore` for none).

Results cross those seams as `Cell`s — node results rendered to their
locator string, value results kept typed — so a raw node id never has
to survive a socket or the JS boundary.

```rust
use quarb_session::{Doc, LocalExecutor, MemStore, Session, Options};

let doc = Doc::open("data.json".as_ref(), &Options::default())?;
let exec = Box::new(LocalExecutor::new(doc, (0, 0), false));
let mut session = Session::new(exec, Box::new(MemStore));
let rows = session.eval("/users/*/name::")?;   // &1
```

The text-format adapters compile by default; the full native fleet
(filesystem, git, SQLite, archives, spreadsheets, source code, mounts)
is behind the `native` feature, and building without it yields the
wasm-safe subset.

[quarb]: https://quarb.org/
[quai]: https://crates.io/crates/quai

## License

Licensed under either of Apache License, Version 2.0 or the MIT license
at your option.
