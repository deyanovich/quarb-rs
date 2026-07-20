# quarb-metatheca

A [metatheca](https://crates.io/crates/metatheca) vault as a quarb
arbor, history first: the state chain is an ancestry axis, every
state carries the full as-of tree, and every entry's children are
its fact events in chain order.

```text
qua 'mt:/path/to/vault' '/paths/docs/a.md/*[::kind = "core/path"] | ::path'
```

## Shape

```text
/
├── head                     alias of the head state
├── states/<hash>            the chain, newest first
│   ├── entries/<uuid>       entries as of that state
│   └── paths/...            the path tree as of that state
├── entries/<uuid>           entries at the head
└── paths/...                the path tree at the head
```

- `/states/<name>` accepts any metatheca stateref without
  enumerating: a full hash, a `>=8`-hex prefix, an ISO-8601
  instant, `~N`, `current`. Quote hop names the lexer can't take
  bare (`/states/'~2'`, instants with `:`); hex prefixes and
  date-only instants work unquoted.
- An entry's children are its fact events (trait `<fact>`),
  oldest first, each stamped `::at` with its introducing state's
  instant. Path-tree leaves alias `/entries/<uuid>` — same
  answers, own position.
- Typed core projections on entries: `::mime`, `::size`,
  `::blob`, `::path`, `::paths`, `::mtime`/`::ctime`/
  `::birthtime`, `::mode`, `::source-path`, `::id`. Any fact
  kind projects generically as `::'ns/name'` (quoted — kinds
  contain `/`): a single-field body unwraps to a typed scalar,
  a multi-field body reads as canonical JSON.
- Crosslinks: state `->previous` / `<-previous` (successor) /
  `::previous~>`; state `->added` (the fact events it
  introduced); fact `->state` / `->entry`.
- Traits: `<state>` `<genesis>` `<head>` `<dir>` `<entry>`
  `<orphan>` (no live path at the coordinate) `<changed>` (a
  fact was added in exactly that coordinate's state) `<fact>`.
- Metadata: states `;;;short` `;;;n-facts` `;;;seq`; entries
  `;;;blob` `;;;size` `;;;n-facts` `;;;n-paths` `;;;first-at`
  `;;;last-at`; facts `;;;hash` `;;;short` `;;;state`
  `;;;state-short` `;;;seq`.

## Exploring the facts history

```text
/states/* @| count                              the chain
/head(->previous)+ @| count                     ancestry walk
/states/'~1'/paths/docs/a.md::                  as-of content read
/states/'2026-07-01T12:00:00Z';;;short          instant stateref
/head(->previous)+[/paths/img.png]? | ;;;short  nearest state where
                                                the file existed
/paths/docs/a.md/*[::kind = "core/path"] | ::path      the renames
/paths/docs/a.md/*[::kind = "core/blob-ref"]::at       every content
                                                       change, dated
/entries/*/*<fact>[::at > '2026-07-01'] @| count       temporal cut
/states/'~2'->added | ::kind                    what a state changed
/states/'~2'/entries/*<changed>                 …as a diff surface
/entries/*<orphan>::path                        the ghosts
```

## Performance

Current-state questions answer from metatheca's persistent SQLite
index. The first genuinely historical touch sweeps the chain —
every fact blob, once, cached for the adapter's lifetime; that is
the same cost as a single `Vault::facts_for` call. Two caveats:

- `/states/*/paths//*` materializes every state's as-of tree —
  O(chain × paths) nodes. For repeated heavy scans, `qua --save`
  materializes the result.
- The adapter raises the quantifier bound to the chain length so
  `(->previous)*` reaches genesis; very long chains make open
  state walks correspondingly long — that is the chain's real
  length, not an adapter artifact.

The head is pinned at open, so one invocation sees one consistent
coordinate system. The adapter never writes.
