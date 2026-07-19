"""Quarb in Jupyter: ``%load_ext quarb.ipython``.

Magics
------

``%quarb_mount PATH [PATH ...] [--descend]``
    Open documents in-process for the session (directories,
    SQLite, kaiv, ``git:PATH``, archives, XLSX, source files, and
    the text formats). One path becomes the default document;
    several are addressed by file stem.

``%quarb_connect NAME PATH [PATH ...]``
    Register a *resident* target set, served by a ``qua
    --resident`` daemon — for arbors too large to rebuild per
    notebook (a whole kernel tree, a huge monorepo). The daemon is
    spawned or reused by qua's own client on the first query;
    later queries answer from the standing arbor. Requires the
    ``qua`` binary on PATH (``cargo install qua``).

``%quarb QUERY`` / ``%%quarb [NAME]``
    Run a query against the default (or named) document or
    resident session. Results render as HTML tables for record
    streams, iterate as typed values (in-process documents keep
    quantities as ``quarb.Quantity``), and convert via ``.df`` /
    ``.df_magnitudes()``.

The in-process engine holds mounted documents for the notebook
session — the same economy ``qua --resident`` buys the CLI —
so re-querying costs no re-parse.
"""

from __future__ import annotations

from .session import QuarbResult, Session  # noqa: F401  (re-export)


def load_ipython_extension(ipython):
    from IPython.core.magic import Magics, line_magic, cell_magic, magics_class

    @magics_class
    class QuarbMagics(Magics):
        def __init__(self, shell):
            super().__init__(shell)
            self.session = Session()

        @line_magic
        def quarb_mount(self, line):
            """%quarb_mount PATH [PATH ...] [--descend]"""
            print(self.session.mount(line))

        @line_magic
        def quarb_connect(self, line):
            """%quarb_connect NAME PATH [PATH ...]"""
            print(self.session.connect(line))

        @line_magic
        def quarb(self, line):
            """%quarb QUERY — run against the default document."""
            return self.session.run(line)

        @cell_magic("quarb")
        def quarb_cell(self, line, cell):
            """%%quarb [NAME] — the cell body is the query."""
            return self.session.run(cell.strip(), line.strip() or None)

    ipython.register_magics(QuarbMagics)
