"""Type stubs for ``quarb._quarb`` — the PyO3 extension module.

The user-facing API is the ``quarb`` module; ``quarb/__init__.py``
re-exports every name listed below.
"""

import os
from typing import Any

__version__: str

class Quantity:
    """A value on a dimension: magnitude in the SI-base expansion
    ``unit`` (e.g. ``m``, ``kg*m^2/s^3``), with the authored form
    kept for display (``written``, e.g. ``'42 km'``) when the
    source carried one."""

    magnitude: float
    unit: str
    written: str | None

class Document:
    """A parsed document: parse once with ``loads``/``load``,
    query many times. Query errors raise ``ValueError``. Bound to
    the creating thread."""

    @property
    def format(self) -> str: ...
    def values(self, query: str) -> list[Any]:
        """The query's values, typed: int, float, str, bool, None,
        ``datetime`` (tz-aware), ``timedelta``, ``Quantity``, dict
        for records, list. A node result renders as
        pointer/locator strings."""

    def value(self, query: str) -> Any:
        """At most one value: the typed value, or None when empty.
        More than one raises ``ValueError``."""

    def records(self, query: str) -> list[dict[str, Any]]:
        """Record results as dicts; a non-record value raises
        ``TypeError``."""

    def nodes(self, query: str) -> list[str]:
        """A node result's pointers/locators; a value result
        raises ``TypeError``."""

def loads(text: str, format: str) -> Document:
    """Parse ``text`` as ``format`` (``json``, ``yaml``, ``toml``,
    ``csv``, ``tsv``, ``xml``, ``html``, ``markdown``)."""

def load(path: str | os.PathLike[str], format: str | None = None) -> Document:
    """Read and parse the file at ``path``; the format is inferred
    from the extension unless given."""

def run(query: str, input: str, format: str) -> list[str]:
    """Execute ``query`` against ``input`` parsed as ``format``;
    result lines as strings (the qua CLI's rendering). Raises
    ``ValueError`` on parse or execution errors."""

def run_file(query: str, path: str | os.PathLike[str]) -> list[str]:
    """Execute ``query`` against the file at ``path``, inferring
    the format from the extension; result lines as strings."""
