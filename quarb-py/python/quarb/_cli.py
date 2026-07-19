"""The ``qua`` command for pip installs — the text-format subset.

The full ``qua`` CLI (files, git, databases, mail, composition)
is the Rust binary: ``cargo install qua``. This entry point covers
what the bindings cover — one query over a text document — so
``pip install qua-cli`` alone yields a working command.
"""

import argparse
import sys

from . import __version__, highlight, run, run_file

FORMATS = "json yaml toml csv tsv xml html markdown".split()


def main(argv=None):
    p = argparse.ArgumentParser(
        prog="qua",
        description=(
            "Structure-aware queries over text formats "
            "(the Quarb engine; text-format subset — "
            "the full CLI is `cargo install qua`)."
        ),
    )
    p.add_argument("query", help="the Quarb query")
    p.add_argument(
        "file",
        nargs="?",
        help="input file (format inferred from the extension); "
        "omit to read stdin, which requires --format",
    )
    p.add_argument(
        "-f",
        "--format",
        choices=FORMATS,
        help="input format (overrides extension inference; "
        "required for stdin)",
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
            if a.format is None:
                p.error("--format is required when reading stdin")
            lines = run(a.query, sys.stdin.read(), a.format)
        elif a.format is not None:
            with open(a.file, encoding="utf-8") as f:
                lines = run(a.query, f.read(), a.format)
        else:
            lines = run_file(a.query, a.file)
    except (ValueError, OSError) as e:
        print(f"qua: {e}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
