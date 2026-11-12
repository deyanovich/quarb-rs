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
import re
import shlex
import subprocess
from pathlib import Path

from . import (
    TABLE_CSS,
    Document,
    Quantity,
    closes_with_table as _closes_with_table,
    closes_with_drawing as _closes_with_drawing,
    diagnostics_html as _diagnostics_html_ext,
    rich_css as _rich_css_ext,
    take_copy as _take_copy,
    mount as _mount,
    open as _open,
    defs_only as _defs_only,
    parse_defs as _parse_defs,
    resolve_previous_line as _resolve_previous_line,
)


class QuarbResult:
    """A query result that displays well and converts willingly.

    The rule every browser host follows holds here too: a result
    whose closing command is ``@| pretty`` renders as the engine's
    HTML grid (``html``, dressed with ``quarb.TABLE_CSS``), and
    everything else stays text — the notebook's rich display
    switches on the same condition, no toggle. ``text`` is the
    table as ``qua`` prints it.
    """

    def __init__(self, values, records, html=None, text=None, diagnostics=None, copy=None):
        self.values = values
        self._records = records
        self.html = html
        self.text = text
        #: The diagnostics the run raised beside its answer (spec:
        #: Diagnostics): dicts with ``severity``, ``code``, ``site``
        #: and ``message``, in raising order. An error is the
        #: exception the run raised instead.
        self.diagnostics = list(diagnostics or [])
        #: ``| copy`` (spec: The clipboard): the rendering's name when
        #: the query asked for its output on the clipboard instead of
        #: the screen; the cell then shows the diagnostic alone.
        self.copy = copy

    def copied_text(self) -> str:
        """The output exactly as it would have printed, what ``| copy``
        puts on the clipboard."""
        return self.text if self.text is not None else "\n".join(str(v) for v in self.values)

    @property
    def records(self):
        return self._records

    def __iter__(self):
        return iter(self.values)

    def __len__(self):
        return len(self.values)

    def __repr__(self):
        if self.copy is not None:
            # the output went to the clipboard: its stand-in is the
            # diagnostic, shown whatever the level
            return "\n".join(_diag_line(d) for d in self.diagnostics
                             if d["code"] == "COPIED" or d["severity"] in ("warning", "error"))
        text = self.text if self.text is not None else "\n".join(str(v) for v in self.values)
        shown = [d for d in self.diagnostics if d["severity"] in ("warning", "error")]
        if shown:
            text += "\n" + "\n".join(_diag_line(d) for d in shown)
        return text

    def _repr_html_(self):
        # A result closing in ``@| pretty`` or in a drawing
        # (``@| tree``, ``@| histogram``) has a rich form; the
        # stylesheet travels with it, so a saved notebook keeps the
        # grid and the drawing dressed. The diagnostics follow the
        # answer: a warning in yellow, the infos (and debug notes)
        # behind a small button that expands them.
        if self.copy is not None:
            return _copy_html(self.copied_text(), self.diagnostics)
        if self.html is None and not self.diagnostics:
            return None
        body = self.html if self.html is not None else (
            "<pre style=\"margin:0\">" + _escape(repr(self) if not self.diagnostics else (
                self.text if self.text is not None else "\n".join(str(v) for v in self.values))) + "</pre>"
        )
        return f"<style>{_rich_css()}</style>\n{body}{_diagnostics_html(self.diagnostics)}"

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


def _diag_line(d) -> str:
    return f"{d['severity']} {d['code']} ({d['site']}): {d['message']}"


def _escape(text: str) -> str:
    import html

    return html.escape(text, quote=False)


def _human_size(n: int) -> str:
    if n < 1000:
        return f"{n} B"
    if n < 1_000_000:
        return f"{n / 1000:.1f} KB"
    return f"{n / 1_000_000:.1f} MB"


def _copy_html(text: str, diagnostics) -> str:
    """``| copy`` in a notebook cell: the text goes to the browser's
    clipboard when the cell renders (a trusted notebook runs the
    script), and a button copies it again on a click, which every
    browser allows; the diagnostic stands in the output's place."""
    payload = json.dumps(text)
    copied = [d for d in diagnostics if d["code"] == "COPIED"]
    line = _escape(copied[0]["message"]) if copied else "copied"
    return (
        f"<style>{TABLE_CSS}</style>\n"
        f"<div class=\"quarb-copy\" style=\"font:13px/1.4 ui-monospace,monospace\">"
        f"<button onclick=\"navigator.clipboard.writeText({_escape(payload)})\" "
        f"style=\"margin-right:8px\">copy</button>"
        f"<span style=\"opacity:.8\"><b>COPIED</b> <span style=\"opacity:.7\">(copy)</span> {line}</span>"
        f"</div>\n"
        f"<script>try{{navigator.clipboard.writeText({payload});}}catch(e){{}}</script>"
        + _diagnostics_html([d for d in diagnostics if d["code"] != "COPIED"])
    )


def _diagnostics_html(diagnostics) -> str:
    """The diagnostics under an answer, from the engine's one renderer
    (``quarb.diagnostics_html``): warnings as lines, the infos and
    debug notes behind a ⓘ ``<details>``; shown down to the info level
    in a notebook, where the stylesheet carries the block's rules."""
    tuples = [
        (d["severity"], d["code"], d["site"], d["message"]) for d in diagnostics
    ]
    return _diagnostics_html_ext(tuples, "info")


def _rich_css() -> str:
    """The stylesheet of every rich form (``quarb.rich_css``)."""
    return _rich_css_ext()


_DIAG_LINE = re.compile(r"^(error|warning|info|debug) ([A-Z_]+) \((.*?)\): (.*)$")


def _parse_diag_lines(text: str) -> list[dict]:
    """The diagnostics a ``qua`` client printed on stderr, one per
    line as ``SEVERITY CODE (site): message``."""
    out = []
    for line in text.splitlines():
        m = _DIAG_LINE.match(line.rstrip())
        if m:
            out.append(dict(zip(("severity", "code", "site", "message"), m.groups())))
    return out


def _diags_of(doc: Document) -> list[dict]:
    return [
        dict(zip(("severity", "code", "site", "message"), d))
        for d in doc.diagnostics()
    ]


def _result(doc: Document, query: str) -> QuarbResult:
    # ``| copy`` comes off the end (spec: The clipboard): the result
    # is built as it would have printed, then handed to the clipboard
    taken = _take_copy(query)
    if taken is not None:
        rest, rendering = taken
        result = _result(doc, rest)
        result.copy = rendering
        size = len(result.copied_text().encode("utf-8"))
        result.diagnostics.append({
            "severity": "info",
            "code": "COPIED",
            "site": "copy",
            "message": f"copied {_human_size(size)} as {rendering}",
        })
        return result
    # the rendering rule (2026-11-09), one implementation in the
    # engine: the values, the text form a ``@| pretty`` closing draws,
    # and the rich HTML form the session's ``--pretty`` setting allows
    # (a table value as its grid, a record stream under ``@| pretty``
    # as the grid, a drawing as its HTML); None keeps the text
    values, text, html = doc.run_rich(query)
    diags = _diags_of(doc)
    if text is not None:
        records = [r for r in values if isinstance(r, dict)]
        return QuarbResult(values, records, html=html, text=text, diagnostics=diags)
    if html is not None and _closes_with_drawing(query):
        text = "\n".join(str(v) for v in values)
        return QuarbResult(values, [], html=html, text=text, diagnostics=diags)
    if values and isinstance(values[0], dict):
        return QuarbResult(values, values, html=html, diagnostics=diags)
    return QuarbResult(values, [], html=html, diagnostics=diags)


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
        # The session's fragment table as definition text, prepended
        # to every query so `&name` refs resolve across cells — the
        # same text-level mechanism quai uses for its `&N` history.
        self.defs_text = ""
        # The cells that ran, as fragments under their labels
        # (`def &7: … ;`), prepended like the table above: `&7` is
        # the query of the cell labelled 7, `&_` the last of them,
        # `&-N` the N-th back (ruling 2026-11-11). The notebook
        # kernel labels a cell by its execution count.
        self.labels: list[int] = []
        self.history_text = ""

    # -- the cells that ran --------------------------------------
    def resolve_previous(self, query: str) -> str:
        """`query` with `&_` / `&-N` (and `(=_)` / `(=-N)`) written
        as the labels they name; raises ValueError where a form
        reaches before the first labelled cell."""
        resolved = _resolve_previous_line(query, self.labels)
        return query if resolved is None else resolved

    def remember(self, label: int, query: str) -> bool:
        """Keep a query that ran as the fragment `&label`.

        False, and nothing kept, where the query cannot stand as a
        fragment's body (it carries definitions of its own) or the
        label is not past the last one: such a cell has no label,
        and `&_` skips it.
        """
        if self.labels and label <= self.labels[-1]:
            return False
        candidate = f"{self.history_text}def &{label}: {query.strip()} ;\n"
        try:
            _parse_defs(f"{self.defs_text}{candidate}")
        except ValueError:
            return False
        self.history_text = candidate
        self.labels.append(label)
        return True

    # -- definitions ---------------------------------------------
    @staticmethod
    def is_defs(text: str) -> bool:
        """Whether `text` is definitions and nothing else.

        A cell whose statements are all `def`/`macro` (comment lines
        aside) extends the session table; a cell that carries a
        query after its definitions runs as a query, which reads the
        leading definitions itself. A cell that is definitions-shaped
        but holds a malformed definition raises that parse error
        here, instead of running as a query and reporting the query
        lexer's confusion (a `#` comment holding an apostrophe read
        as "unterminated quoted name").
        """
        head = text.lstrip()
        if not (head.startswith("def ") or head.startswith("macro ")
                or head.startswith("#")):
            return False
        return _defs_only(text)

    def add_defs(self, text: str) -> str:
        """Add `def`/`macro` statements to the session's table.

        Validated against the whole standing table first, so a bad
        cell leaves the table untouched. The names defined here
        resolve in every later query, whichever document it runs
        against. Stored stripped of `#` comment lines: the table is
        prepended to query text, and the query lexer itself has no
        comment syntax.
        """
        stripped = "\n".join(
            "" if line.lstrip().startswith("#") else line
            for line in text.splitlines()
        )
        candidate = f"{self.defs_text}{stripped}\n"
        _parse_defs(candidate)  # raises ValueError with the position
        self.defs_text = candidate
        names = [
            tok.split("(")[0].split(":")[0].strip()
            for line in text.splitlines()
            for tok in [line.strip().removeprefix("def ").removeprefix("macro ")]
            if line.strip().startswith(("def ", "macro "))
        ]
        return "defined: " + ", ".join(n for n in names if n)

    # -- mounting ------------------------------------------------
    def mount(self, line: str) -> str:
        args = shlex.split(line)
        # --descend, the pre-0.24 spelling of --graft, is refused
        # with the pointer, as qua refuses it.
        if "--descend" in args:
            raise ValueError("--descend is retired: write --graft")
        graft = "--graft" in args
        no_graft = "--no-graft" in args
        flags = {"--graft", "--no-graft"}
        paths = [a for a in args if a not in flags]
        if graft and no_graft:
            raise ValueError("--graft and --no-graft conflict")
        if not paths:
            raise ValueError(
                "usage: mount PATH [PATH ...] [--graft | --no-graft]"
            )
        if len(paths) == 1:
            name = Path(paths[0].removeprefix("git:")).stem or paths[0]
            self.docs[name] = _open(paths[0], graft, no_graft)
            self.default = name
            return f"mounted: {name}"
        # Several paths mount together under one root, so a single
        # query can join across them (/a/... <=> /b/...). Keyed by
        # the joined stems.
        stems = [Path(p.removeprefix("git:")).stem or p for p in paths]
        name = "+".join(stems)
        self.docs[name] = _mount(paths, graft, no_graft)
        self.default = name
        return f"mounted: {name} (children: {', '.join('/' + s for s in stems)})"

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
        query = f"{parent}/*:name" if parent else "/*:name"
        try:
            names = [str(n) for n in doc.values(query)]
        except Exception:
            return [], cursor_pos, cursor_pos
        matches = sorted({n for n in names if n.startswith(seg) and n != ""})
        return matches, slash + 1, cursor_pos

    def run(self, query: str, name: str | None = None) -> QuarbResult:
        picked = self._pick(name)
        if self.defs_text or self.history_text:
            # Prepend the fragment table and the labelled cells; the
            # engine takes inline defs ahead of the query in one text.
            query = f"{self.defs_text}{self.history_text}{query}"
        if picked in self.resident:
            argv = [self.qua, "--resident", query, *self.resident[picked]]
            proc = subprocess.run(argv, capture_output=True, text=True)
            if proc.returncode != 0:
                raise RuntimeError(proc.stderr.strip() or "qua --resident failed")
            lines = proc.stdout.splitlines()
            result = _lines_result(lines)
            # The client prints the run's diagnostics on its stderr,
            # one per line; they belong to this result.
            result.diagnostics = _parse_diag_lines(proc.stderr)
            return result
        return _result(self.docs[picked], query)
