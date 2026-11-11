"""The ``qua`` command for pip installs: one query over one input.

The full ``qua`` CLI (several mounts, the networked adapters, mail,
composition, every flag) is the Rust binary, shipped prebuilt under
the same command name by ``pip install quarb-full`` (or built from
source with ``cargo install qua``). This entry point covers what
the bindings cover, and reads its input as the full CLI does:
standard input is JSON unless ``--format`` names another notation,
and a path opens as ``quarb.open`` opens it — a text file by its
extension; a directory, a SQLite database, a kaiv file, a git
repository (``git:PATH``), an archive, a workbook, a source file.
"""

import argparse
import sys

from . import __version__, highlight, run, run_file
from . import open as open_path

# The notations of standard input, under the names the full CLI
# gives them; the common ones have a flag of their own there too.
FORMATS = "json jsonl yaml toml xml html markdown text csv tsv conllu".split()
DIRECT = "yaml toml xml html markdown text".split()


def lines_of(query, path):
    """The result lines of ``query`` over ``path``: a text file by
    its extension, any other path as ``quarb.open`` opens it."""
    try:
        return run_file(query, path)
    except ValueError as e:
        # no text format by that extension (or none at all: a
        # directory, a database, ``git:PATH``): the path's kind decides
        if not str(e).startswith("cannot infer format"):
            raise
    return open_path(str(path)).lines(query)


def main(argv=None):
    p = argparse.ArgumentParser(
        prog="qua",
        description=(
            "Structure-aware queries over one input "
            "(the Quarb engine as the Python package carries it; "
            "the full CLI is `pip install quarb-full`)."
        ),
    )
    p.add_argument("query", help="the Quarb query")
    p.add_argument(
        "file",
        nargs="?",
        help="the input: a text file (format by its extension), or "
        "whatever quarb.open opens (a directory, SQLite, kaiv, "
        "git:PATH, an archive, a workbook, a source file); "
        "omit to read standard input",
    )
    p.add_argument(
        "-f",
        "--format",
        choices=FORMATS,
        help="the notation of the input, over its extension; "
        "standard input is read as JSON by default and never sniffed",
    )
    for name in DIRECT:
        p.add_argument(
            f"--{name}",
            dest="format",
            action="store_const",
            const=name,
            help=f"the same as --format {name}",
        )
    p.add_argument(
        "--highlight",
        action="store_true",
        help="print the query with ANSI syntax highlighting and exit",
    )
    p.add_argument(
        "--version",
        action="version",
        version=f"qua (quarb {__version__})",
    )
    a = p.parse_args(argv)
    if a.highlight:
        import os
        if os.environ.get("NO_COLOR") is not None:
            print(a.query)
        else:
            print(highlight(a.query))
        return 0
    try:
        if a.file is None:
            lines = run(a.query, sys.stdin.read(), a.format or "json")
        elif a.format is not None:
            with open(a.file, encoding="utf-8") as f:
                lines = run(a.query, f.read(), a.format)
        else:
            lines = lines_of(a.query, a.file)
    except (ValueError, OSError) as e:
        print(f"qua: {e}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
