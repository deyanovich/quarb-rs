# quarb-full

The **complete `qua` CLI, prebuilt** — every Quarb adapter, no
Rust toolchain, no compile:

```sh
pip install quarb-full
qua '/torvalds/linux::stars' github:
qua '/gitlab-org//*<project>::name' gitlab:
```

Part of [Quarb](https://quarb.org), a query language for
*arbors* — tree-spanned graphs. This package ships the full
reference CLI as a prebuilt binary: PostgreSQL, MySQL, DuckDB,
BigQuery, MongoDB, Neo4j, Firebase/Firestore/Datastore,
Kubernetes, GitHub, GitLab, IMAP/Maildir, object stores, git,
the filesystem, composition and mounts — the [full adapter
list](https://quarb.org/adapters/).

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
