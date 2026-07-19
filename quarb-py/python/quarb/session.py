"""Session state shared by the IPython magics and the Quarb kernel.

A :class:`Session` holds named mounted documents (in-process, via
:func:`quarb.open`) and optional *resident* targets — file sets
served by a ``qua --resident`` daemon for arbors too large to
rebuild per notebook (the CLI's own client logic handles daemon
spawn/reuse; Python just runs ``qua``).

:class:`QuarbResult` is the display side: record streams render
as HTML tables, everything iterates as typed values, and pandas
is two properties away.
"""

from __future__ import annotations

import json
import shlex
import subprocess
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


def _result(doc: Document, query: str) -> QuarbResult:
    values = doc.values(query)
    if values and isinstance(values[0], dict):
        return QuarbResult(values, values)
    return QuarbResult(values, [])


def _lines_result(lines: list[str]) -> QuarbResult:
    """Wrap rendered CLI output: JSONL lines parse into records."""
    records = []
    for line in lines:
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            try:
                records.append(json.loads(line))
                continue
            except ValueError:
                pass
        records = []
        break
    else:
        if records and len(records) == len([l for l in lines if l.strip()]):
            return QuarbResult(records, records)
    return QuarbResult(lines, [])


class Session:
    """Named documents plus optional resident target sets."""

    def __init__(self, qua: str = "qua"):
        self.docs: dict[str, Document] = {}
        self.resident: dict[str, list[str]] = {}
        self.default: str | None = None
        self.qua = qua

    # -- mounting ------------------------------------------------
    def mount(self, line: str) -> str:
        args = shlex.split(line)
        descend = "--descend" in args
        paths = [a for a in args if a != "--descend"]
        if not paths:
            raise ValueError("usage: mount PATH [PATH ...] [--descend]")
        names = []
        for p in paths:
            name = Path(p.removeprefix("git:")).stem or p
            self.docs[name] = _open(p, descend)
            names.append(name)
        if len(paths) == 1:
            self.default = names[0]
        return "mounted: " + ", ".join(sorted(self.docs))

    def connect(self, line: str) -> str:
        """Register a resident target set (served by qua --resident).

        The daemon is spawned or reused by the qua client itself on
        the first query; repeated queries hit the standing arbor.
        """
        args = shlex.split(line)
        if not args:
            raise ValueError("usage: connect NAME PATH [PATH ...] [FLAGS]")
        name, rest = args[0], args[1:]
        if not rest:
            raise ValueError("usage: connect NAME PATH [PATH ...] [FLAGS]")
        self.resident[name] = rest
        self.default = name
        return f"resident session {name!r}: {' '.join(rest)}"

    # -- running -------------------------------------------------
    def _pick(self, name: str | None) -> str:
        if name:
            if name not in self.docs and name not in self.resident:
                have = ", ".join(sorted([*self.docs, *self.resident])) or "none"
                raise KeyError(f"no document {name!r} (have: {have})")
            return name
        if self.default:
            return self.default
        pool = [*self.docs, *self.resident]
        if len(pool) == 1:
            return pool[0]
        raise RuntimeError(
            "no default document — mount a single path, or name one"
        )

    def complete(self, text: str, cursor_pos: int):
        """Path completions from the live default document.

        Given the query text and cursor, complete the path segment
        under the cursor against the mounted arbor's real child
        names — ``/users/*/na`` offers ``name`` because the data
        has it. Returns ``(matches, start, end)`` (end == cursor).
        Resident sessions and non-navigable spots yield nothing.
        """
        try:
            name = self._pick(None)
        except Exception:
            return [], cursor_pos, cursor_pos
        if name not in self.docs:
            return [], cursor_pos, cursor_pos
        doc = self.docs[name]
        # The segment under the cursor: back to the last '/', but a
        # path only navigates from a '/', not from inside a literal.
        left = text[:cursor_pos]
        slash = left.rfind("/")
        if slash < 0:
            return [], cursor_pos, cursor_pos
        seg = left[slash + 1 :]
        stops = set(" \t\"'()[]|")
        if any(c in stops for c in seg):
            return [], cursor_pos, cursor_pos
        parent = left[:slash]
        # Trim any pipe/paren-scoped fragment back to the path root.
        for i in range(len(parent) - 1, -1, -1):
            if parent[i] in " \t|(":
                parent = parent[i + 1 :]
                break
        # Inside an unclosed predicate `[ ... `, the path is relative
        # to the node the predicate hangs on: splice the enclosing
        # path onto the relative fragment.
        if parent.count("[") > parent.count("]"):
            base, _, rel = parent.rpartition("[")
            parent = base.rstrip("/") + rel
        parent = parent.rstrip("/")
        # `parent` is now a path like "/users/*" or "" (root).
        query = f"{parent}/*:::name" if parent else "/*:::name"
        try:
            names = [str(n) for n in doc.values(query)]
        except Exception:
            return [], cursor_pos, cursor_pos
        matches = sorted({n for n in names if n.startswith(seg) and n != ""})
        return matches, slash + 1, cursor_pos

    def run(self, query: str, name: str | None = None) -> QuarbResult:
        picked = self._pick(name)
        if picked in self.resident:
            argv = [self.qua, "--resident", query, *self.resident[picked]]
            proc = subprocess.run(argv, capture_output=True, text=True)
            if proc.returncode != 0:
                raise RuntimeError(proc.stderr.strip() or "qua --resident failed")
            lines = proc.stdout.splitlines()
            return _lines_result(lines)
        return _result(self.docs[picked], query)
