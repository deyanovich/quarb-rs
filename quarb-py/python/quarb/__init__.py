"""quarb — structure-aware queries over text formats.

Python bindings for the Quarb query engine. Quarb reads structured
text as an *arbor* (a tree-spanned graph) and runs one query
language over all of it — generalizing what XPath does for XML and
jq does for JSON to JSON, YAML, TOML, CSV/TSV, XML, HTML, and
Markdown alike.

Docs: https://quarb.org/  ·  Playground: https://demo.quarb.org/

Quick start::

    import quarb

    doc = quarb.loads('''{
      "books": [
        {"title": "Emma", "price": 22.5},
        {"title": "Iliad", "price": 9.99}
      ]
    }''', "json")

    doc.values('/books/*/title::')        # ['Emma', 'Iliad']
    doc.values('/books/*/price::')        # [22.5, 9.99] — floats
    doc.value('/books/*/price:: @| mean') # 16.245
    doc.records('/books/* | rec("t", /title::)')
    #                                # [{'t': 'Emma'}, {'t': 'Iliad'}]

``loads(text, format)`` / ``load(path)`` parse once into a
:class:`Document`, queried many times; results come back *typed* —
ints, floats, ``datetime``, ``timedelta``, :class:`Quantity`,
dicts for records, ``None`` for null. ``run`` / ``run_file`` are
the low-level layer: result lines as strings, exactly as the
``qua`` CLI renders them. Errors raise ``ValueError`` with the
engine's message.

``doc.plan(query)`` prices a query before it runs: the static plan
as a kaiv payload (per step, the nodes and bytes it may reach as
ranges), and ``budget="nodes=1M,bytes=2GiB"`` on ``values`` /
``value`` / ``records`` / ``nodes`` refuses a query whose plan
passes the bound before anything is read (``strict`` refuses an
unknown estimate too) — the ``qua --plan`` / ``--budget`` pair.

A query whose closing command is ``@| table`` has a second form:
``doc.table(query)`` returns ``(rows, text, html)`` — the rows as
typed values, the text table as ``qua`` prints it, and the HTML
grid every browser host renders, dressed with ``quarb.TABLE_CSS``.
In a notebook the :class:`quarb.QuarbResult` of such a query
displays as that grid; everything else displays as text.

``quarb.open(path)`` dispatches on the path's kind — directories
(``graft=True`` grafts parseable leaves), SQLite, kaiv/daiv
(typed units, instants, durations), git repositories (``git:PATH``),
archives, XLSX workbooks, and source files — and
``quarb.translate(src, "jq"|"xpath"|"sql")`` turns queries you
already have into Quarb text.

The ``qua`` console command installed with this package covers the
same text formats; the full CLI (files, git, databases, mail) is
the Rust binary: ``cargo install qua``.
"""

from . import _quarb as _ext

__version__ = _ext.__version__

# The pythonic layer: parse once, query many, typed results.
Document = _ext.Document
Quantity = _ext.Quantity
load = _ext.load
loads = _ext.loads
open = _ext.open
mount = _ext.mount
translate = _ext.translate
highlight = _ext.highlight
closes_with_table = _ext.closes_with_table
TABLE_CSS = _ext.TABLE_CSS
parse_defs = _ext.parse_defs
defs_only = _ext.defs_only
man = _ext.man

# The string-faithful layer: the qua CLI's exact rendering.
run = _ext.run
run_file = _ext.run_file

__all__ = [
    "open",
    "mount",
    "translate",
    "highlight",
    "parse_defs",
    "defs_only",
    "man",
    "__version__",
    "Document",
    "Quantity",
    "load",
    "loads",
    "run",
    "run_file",
]
