"""The interactive session: a macro table where each accepted line
becomes ``def &N: <line> ;``, evaluated through the :mod:`quarb`
engine.

History is the language's own reuse mechanism, not a bolted-on cell
store: line 3 is the fragment ``&3``, continued through the pipe
(``&3 | /name::``, ``&3 | [pred]``, ``&3 @| count``). A standalone
``&N#`` replays a line's captured output (the frozen footprint); in
this pure-Python front end the source is materialized once, so
``&N``, ``&N!`` and ``&N#`` coincide for a fixed document.
"""

from __future__ import annotations

import quarb


class Session:
    def __init__(self, doc: "quarb.Document"):
        self.doc = doc
        # The macro table as definition text, prepended to each query
        # so ``&N`` history resolves inline.
        self.defs_text = ""
        # Each line's output, captured at commit — what ``&N#`` replays.
        self.snapshots: dict[int, list] = {}
        # The next line's number — the ``&N`` a fresh line will claim.
        self.line_no = 1

    def _combined(self, line: str) -> str:
        if not self.defs_text:
            return line
        return f"{self.defs_text}\n{line}"

    def eval(self, line: str) -> list:
        """Run a line against the source with the current macro table
        prepended. Node results come back as locator strings, value
        results as typed Python values (dicts for records)."""
        return self.doc.values(self._combined(line))

    def commit(self, line: str, snapshot: list) -> bool:
        """Register an accepted line as ``&N`` and capture its output as
        the frozen footprint for ``&N#``. Returns whether the line's
        shape can be a macro body (so ``&N`` will resolve); either way
        the line number advances so labels track what the user saw."""
        self.snapshots[self.line_no] = list(snapshot)
        # A space before the ``;`` terminator: a line ending in a ``::``
        # projection would otherwise lex ``::;`` as the metadata sigil.
        candidate = f"{self.defs_text}def &{self.line_no}: {line} ;\n"
        referenceable = self._defs_ok(candidate)
        if referenceable:
            self.defs_text = candidate
        self.line_no += 1
        return referenceable

    def _defs_ok(self, candidate: str) -> bool:
        # Validate by running the candidate table plus a reference to
        # the new line; a parse error means it cannot be a macro body.
        try:
            self.doc.values(f"{candidate}&{self.line_no}")
            return True
        except Exception:
            return False

    def frozen(self, n: int):
        """The frozen output of line ``n``, if captured."""
        return self.snapshots.get(n)

    def record_frozen(self, snapshot: list) -> None:
        """Record a frozen-recall line: it takes the next number and
        keeps its own snapshot, but is not a referenceable macro body."""
        self.snapshots[self.line_no] = list(snapshot)
        self.line_no += 1

    def add_def(self, line: str) -> None:
        """Add a ``def``/``macro`` line to the table (validated first)."""
        candidate = f"{self.defs_text}{line}\n"
        # Parse the whole table by running it with a trivial trailing
        # query (``/`` — the root — always parses); a malformed
        # definition raises here, before it can poison later lines.
        self.doc.values(f"{candidate}/")
        self.defs_text = candidate

    def history(self) -> str:
        return self.defs_text

    def reset(self) -> None:
        self.defs_text = ""
        self.snapshots = {}
        self.line_no = 1
