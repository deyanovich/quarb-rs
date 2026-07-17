"""Type stubs for ``quarb._quarb`` — the PyO3 extension module.

The user-facing API is the ``quarb`` module; ``quarb/__init__.py``
re-exports every name listed below.
"""

import os

__version__: str

def run(query: str, input: str, format: str) -> list[str]:
    """Execute ``query`` against ``input`` parsed as ``format``.

    ``format`` is one of ``json``, ``yaml``, ``toml``, ``csv``,
    ``tsv``, ``xml``, ``html``, ``markdown``. Returns the result
    lines; raises ``ValueError`` on parse or execution errors.
    """

def run_file(query: str, path: str | os.PathLike[str]) -> list[str]:
    """Execute ``query`` against the file at ``path``.

    The format is inferred from the extension (``.json``,
    ``.yaml``/``.yml``, ``.toml``, ``.csv``, ``.tsv``, ``.xml``,
    ``.html``/``.htm``, ``.md``/``.markdown``); an unknown extension
    raises ``ValueError``.
    """
