"""quarb — structure-aware queries over text formats.

Python bindings for the Quarb query engine. Quarb reads structured
text as an *arbor* (a tree-spanned graph) and runs one query
language over all of it — generalizing what XPath does for XML and
jq does for JSON to JSON, YAML, TOML, CSV/TSV, XML, HTML, and
Markdown alike.

Docs: https://quarb.org/  ·  Playground: https://demo.quarb.org/

Quick start::

    import quarb

    doc = '{"books": [{"title": "Sapiens", "price": 25}]}'
    quarb.run('/books/*/title::', doc, 'json')
    # ['Sapiens']

``run`` takes the input as a string plus an explicit format name;
``run_file`` reads a file and infers the format from its extension.
Both return the result lines as a list of strings — node results
render through the format's pointer/locator, value results through
their display form — and raise ``ValueError`` with the engine's
message on parse or execution errors.
"""

from . import _quarb as _ext

__version__ = _ext.__version__

# Functions
run = _ext.run
run_file = _ext.run_file

__all__ = [
    "__version__",
    "run",
    "run_file",
]
