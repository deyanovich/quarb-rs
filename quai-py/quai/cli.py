"""The ``quai`` command: an interactive Quarb REPL.

Built on :func:`input`, with the :mod:`readline` module imported so
backspace, arrow keys, and input-line history work out of the box. The
color codes in the prompt are wrapped in readline's zero-width markers
(``\\001``/``\\002``) so cursor math near the prompt stays correct.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

try:
    import readline  # noqa: F401 — enables line editing for input()
except ImportError:  # pragma: no cover — e.g. Windows without pyreadline
    pass

import quarb

from .session import Session

_CYAN = "\001\x1b[36m\002"
_RESET = "\001\x1b[0m\002"


def main() -> None:
    ap = argparse.ArgumentParser(
        prog="quai",
        description="Interactive Quarb — each line becomes a reusable "
        "query (&1, &2, ...).",
    )
    ap.add_argument(
        "paths",
        nargs="*",
        help="source(s): a document (.json/.yaml/.toml/.csv/.xml/.html/"
        ".md), a SQLite/kaiv/xlsx file, a directory, or git:PATH. "
        "Several mount as one root, so a single query — including a "
        "<=> join — spans them all.",
    )
    args = ap.parse_args()
    if not args.paths:
        ap.error("quai needs at least one source (a document, a directory, or git:PATH)")

    try:
        doc = (
            quarb.open(args.paths[0])
            if len(args.paths) == 1
            else quarb.mount(args.paths)
        )
    except Exception as e:  # noqa: BLE001 — surface the engine's message
        sys.exit(f"quai: {e}")

    session = Session(doc)
    color = sys.stdout.isatty() and not os.environ.get("NO_COLOR")
    print(
        f"quai - interactive Quarb over {', '.join(args.paths)}.  "
        ":help for commands, :quit (or Ctrl-D) to leave."
    )
    _repl(session, color)


def _repl(session: Session, color: bool) -> None:
    while True:
        prompt = (
            f"{_CYAN}&{session.line_no}{_RESET} " if color else f"&{session.line_no} "
        )
        try:
            line = input(prompt)
        except EOFError:
            print()
            break
        except KeyboardInterrupt:
            print()
            continue
        line = line.strip()
        if not line:
            continue

        # A ':' command (a query cannot start with a lone ':').
        if line.startswith(":") and not line.startswith("::"):
            if _command(session, line):
                break
            continue

        # A definition extends the macro table but is not itself run.
        if (
            line.startswith("def ")
            or line == "def"
            or line.startswith("macro ")
            or line == "macro"
        ):
            try:
                session.add_def(line)
            except Exception as e:  # noqa: BLE001
                print(f"error: {e}", file=sys.stderr)
            continue

        # Capture references (&N#, &N!) are resolved here, not by the
        # engine (its lexer has no '#', and '!' marks data-aware macros).
        try:
            kind, arg = _prepare(line)
        except ValueError as e:
            print(f"error: {e}", file=sys.stderr)
            continue

        if kind == "frozen":
            snap = session.frozen(arg)
            if snap is None:
                print(
                    f"error: &{arg}# has no captured result (line {arg} hasn't run)",
                    file=sys.stderr,
                )
            else:
                for v in snap:
                    print(_render(v))
                session.record_frozen(snap)
            continue

        try:
            values = session.eval(arg)
        except Exception as e:  # noqa: BLE001
            print(f"error: {e}", file=sys.stderr)
            continue
        for v in values:
            print(_render(v))
        n = session.line_no
        if not session.commit(arg, values):
            print(
                f"note: &{n} is not referenceable (its shape can't be a macro body)",
                file=sys.stderr,
            )


def _prepare(line: str):
    n = _numeric_ref(line, "#")
    if n is not None:
        return ("frozen", n)
    n = _numeric_ref(line, "!")
    if n is not None:
        # Live re-run; for a fixed in-process document this equals &N.
        return ("eval", f"&{n}")
    if "#" in line:
        raise ValueError(
            "'#' is the frozen-history suffix, valid only as a standalone '&N#'"
        )
    return ("eval", line)


def _numeric_ref(line: str, suffix: str):
    if line.endswith(suffix) and line.startswith("&"):
        body = line[1:-1]
        if body.isdigit():
            return int(body)
    return None


def _render(v) -> str:
    if isinstance(v, dict):
        return json.dumps(v, ensure_ascii=False)
    return str(v)


def _command(session: Session, line: str) -> bool:
    if line in (":q", ":quit"):
        return True
    if line in (":help", ":?"):
        print(
            "  <query>       run a query; its result is labelled &N and reusable\n"
            "  &N            re-run line N (a macro); continue with a pipe: &N | /key::\n"
            "  &N#           replay line N's frozen output (as it was when it ran)\n"
            "  def &x: ... ; add a named fragment to the session\n"
            "  :history      show the macro table (&1, &2, ...)\n"
            "  :reset        clear the history and restart numbering\n"
            "  :quit         leave (also Ctrl-D)"
        )
    elif line == ":history":
        h = session.history().strip()
        print(h if h else "(no history yet)")
    elif line == ":reset":
        session.reset()
    else:
        print(f"unknown command '{line}' (:help lists them)")
    return False


if __name__ == "__main__":
    main()
