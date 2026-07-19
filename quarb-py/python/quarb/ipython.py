"""Quarb in Jupyter: ``%load_ext quarb.ipython``.

Magics
------

``%quarb_mount PATH [PATH ...] [--descend]``
    Open documents for the session (the full local fleet:
    directories, SQLite, kaiv, ``git:PATH``, archives, XLSX,
    source files, and the text formats). One path becomes the
    default document; several are addressed by file stem.

``%quarb QUERY`` / ``%%quarb [NAME]``
    Run a query against the default (or named) document. The
    result renders as an HTML table for record streams and as
    plain lines otherwise, and is also a value: ``.values`` is
    the typed list, ``.records`` the list of dicts, and ``.df``
    a pandas DataFrame (pandas required only for ``.df``).

Typed values pass through untouched: quantities arrive as
``quarb.Quantity`` (magnitude/unit/written), so nothing is
silently flattened. ``.df`` keeps those objects in the frame;
call ``.df_magnitudes()`` for a numbers-only frame where each
quantity contributes its base magnitude.

The engine runs in-process — mounted documents live for the
notebook session, so re-querying costs no re-parse (the same
economy ``qua --resident`` buys the CLI).
"""

from __future__ import annotations

import shlex
from html import escape
from pathlib import Path

from . import Document, Quantity, open as _open


class QuarbResult:
    """A query result that displays well and converts willingly."""

    def __init__(self, values, records):
        self.values = values
        self._records = records

    @property
    def records(self):
        return self._records

    def __iter__(self):
        return iter(self.values)

    def __len__(self):
        return len(self.values)

    def __repr__(self):
        return "\n".join(str(v) for v in self.values)

    def _repr_html_(self):
        if not self._records:
            body = "<br>".join(escape(str(v)) for v in self.values)
            return f'<div style="font-family:monospace">{body}</div>'
        cols: list[str] = []
        for r in self._records:
            for k in r:
                if k not in cols:
                    cols.append(k)
        head = "".join(
            f'<th style="text-align:left;border-bottom:2px solid #8a2434;'
            f'padding:2px 10px">{escape(c)}</th>'
            for c in cols
        )
        rows = "".join(
            "<tr>"
            + "".join(
                f'<td style="padding:2px 10px;border-bottom:1px solid #ddd">'
                f'{escape(str(r.get(c, "")))}</td>'
                for c in cols
            )
            + "</tr>"
            for r in self._records
        )
        return (
            '<table style="border-collapse:collapse;font-size:0.9em">'
            f"<thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table>"
        )

    @property
    def df(self):
        import pandas as pd

        if self._records:
            return pd.DataFrame(self._records)
        return pd.Series(self.values).to_frame("value")

    def df_magnitudes(self):
        """The DataFrame with quantities reduced to base magnitudes."""
        import pandas as pd

        def flat(v):
            return v.magnitude if isinstance(v, Quantity) else v

        if self._records:
            return pd.DataFrame(
                [{k: flat(v) for k, v in r.items()} for r in self._records]
            )
        return pd.Series([flat(v) for v in self.values]).to_frame("value")


def _run(doc: Document, query: str) -> QuarbResult:
    values = doc.values(query)
    try:
        records = doc.records(query)
    except Exception:
        records = []
    # records() mirrors values() for record streams; anything else
    # (plain scalars, nodes) renders as lines.
    if not (values and isinstance(values[0], dict)):
        records = []
    return QuarbResult(values, records)


def load_ipython_extension(ipython):
    from IPython.core.magic import Magics, line_magic, cell_magic, magics_class

    @magics_class
    class QuarbMagics(Magics):
        def __init__(self, shell):
            super().__init__(shell)
            self.docs: dict[str, Document] = {}
            self.default: Document | None = None

        @line_magic
        def quarb_mount(self, line):
            """%quarb_mount PATH [PATH ...] [--descend]"""
            args = shlex.split(line)
            descend = "--descend" in args
            paths = [a for a in args if a != "--descend"]
            if not paths:
                print("usage: %quarb_mount PATH [PATH ...] [--descend]")
                return
            for p in paths:
                name = Path(p.removeprefix("git:")).stem or p
                self.docs[name] = _open(p, descend)
                self.default = self.docs[name] if len(paths) == 1 else self.default
            if len(paths) == 1:
                self.default = self.docs[Path(paths[0].removeprefix("git:")).stem]
            mounted = ", ".join(sorted(self.docs))
            print(f"mounted: {mounted}")

        def _doc(self, name: str | None) -> Document:
            if name:
                if name not in self.docs:
                    raise KeyError(
                        f"no document {name!r} mounted "
                        f"(have: {', '.join(sorted(self.docs)) or 'none'})"
                    )
                return self.docs[name]
            if self.default is None:
                if len(self.docs) == 1:
                    return next(iter(self.docs.values()))
                raise RuntimeError(
                    "no default document — %quarb_mount one path, or name "
                    "one: %%quarb NAME"
                )
            return self.default

        @line_magic
        def quarb(self, line):
            """%quarb QUERY — run against the default document."""
            return _run(self._doc(None), line)

        @cell_magic("quarb")
        def quarb_cell(self, line, cell):
            """%%quarb [NAME] — the cell body is the query."""
            name = line.strip() or None
            return _run(self._doc(name), cell.strip())

    ipython.register_magics(QuarbMagics)
