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
        {"title": "Sapiens", "price": 22.5},
        {"title": "Dune", "price": 9.99}
      ]
    }''', "json")

    doc.values('/books/*/title::')        # ['Sapiens', 'Dune']
    doc.values('/books/*/price::')        # [22.5, 9.99] — floats
    doc.value('/books/*/price:: @| mean') # 16.245
    doc.records('/books/* | rec("t", /title::)')
    #                                # [{'t': 'Sapiens'}, {'t': 'Dune'}]

``loads(text, format)`` / ``load(path)`` parse once into a
:class:`Document`, queried many times; results come back *typed* —
ints, floats, ``datetime``, ``timedelta``, :class:`Quantity`,
dicts for records, ``None`` for null. ``run`` / ``run_file`` are
the low-level layer: result lines as strings, exactly as the
``qua`` CLI renders them. Errors raise ``ValueError`` with the
engine's message.

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

# The string-faithful layer: the qua CLI's exact rendering.
run = _ext.run
run_file = _ext.run_file

__all__ = [
    "__version__",
    "Document",
    "Quantity",
    "load",
    "loads",
    "run",
    "run_file",
]
