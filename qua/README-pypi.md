# quarb-full

The **complete `qua` CLI, prebuilt** — every Quarb adapter, no
Rust toolchain, no compile:

```sh
pip install quarb-full
qua '/torvalds/linux::stars' github:
qua '/gitlab-org//*<project>::name' gitlab:
qua '/* | %(::species; ::bill_length_mm; ::flipper_length_mm) @| sklearn:cluster.KMeans.fit_predict(3)' penguins.csv
```

The Python command libraries come along: `np:`, `pd:`, `sp:`,
`sklearn:` and `morph:` (numpy, pandas, scipy, scikit-learn,
pymorphy3) are linked in, and the packages they wrap are
dependencies of this wheel, so one `pip install` gives a `qua`
that runs them in a stateless call, no notebook, no script. The
binary embeds the CPython it was built for (the wheel is tagged
per version) and finds the packages where pip put them: in the
same environment as the wheel, or in an active virtualenv.

Part of [Quarb](https://quarb.org), a query language for
*arbors* — tree-spanned graphs. This package ships the full
reference CLI as a prebuilt binary: PostgreSQL, MySQL, DuckDB,
BigQuery, MongoDB, Neo4j, Firebase/Firestore/Datastore,
Kubernetes, GitHub, GitLab, IMAP/Maildir, object stores, git,
the filesystem, composition and mounts — the [full adapter
list](https://quarb.org/adapters.html).

The base [`quarb`](https://pypi.org/project/quarb/) package is
the other half: Python bindings (`import quarb`, the Jupyter
kernel) plus a lightweight text-format `qua`. Both packages
install the same `qua` command — the last one installed wins,
so after upgrading `quarb`, re-run `pip install
--force-reinstall quarb-full` if the full CLI should keep the
name.

See [quarb.org](https://quarb.org) for the language
specification, the user guide, and the cookbooks.

## License

Dual-licensed under either of MIT or Apache-2.0, at your option.
