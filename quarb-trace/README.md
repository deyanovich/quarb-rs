# quarb-trace

The `quarb/trace` viz contract as Rust: the payload types a
[Quarb][quarb] trace carries — the arbor snapshot, the query's
steps, the threads, the evaluation snapshots, positions, ribbons,
lookahead verdicts, the window and its elision stubs — and the
writer that emits them as a [kaiv][kaiv] document.

The contract itself is the kaiv schema, typed from a corpus of
payloads; this crate is its Rust embodiment, nothing more. The
engine fills a `Payload` (`quarb::trace::trace`, or
`quarb_session::Doc::trace`) and renderers read the kaiv document
it writes — the terminal renderer `quarbopsis-tui`, the browser
renderer to come, or any third party's. No engine dependency, no
reader, no dependencies at all.

```rust
let doc = quarb_session::Doc::parse(html, "html")?;
let opts = quarb::trace::Options {
    adapter: "html".into(),
    source: "page.html".into(),
    ..Default::default()
};
let payload = doc.trace("//nav//a[::class]::href", None, &opts)?;
std::fs::write("trace.kaiv", payload.to_kaiv())?;
```

A staged suffix (`Some("| [::href == (/\\.pdf$/)]")`) previews a
step without committing it: the payload's working frame is the one
before it, with the staged step's verdicts as that frame's
lookahead — a filter preview. `window` and `measures` select the
slice of a large result list to embed and what the stubs on either
side report.

[quarb]: https://quarb.org/
[kaiv]: https://kaiv.io/

## License

Licensed under either of Apache License, Version 2.0 or the MIT license
at your option.
