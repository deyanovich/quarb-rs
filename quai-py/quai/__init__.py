"""Interactive Quarb — the ``quai`` session REPL.

A thin, pure-Python front end over the :mod:`quarb` engine: it opens a
source, holds a session, and labels each accepted line ``&1``, ``&2``,
… — every label a reusable macro (the *query*, not the printed value),
ready to continue through the pipe. ``pip install quai`` gives you the
``quai`` command; the engine itself rides in on the ``quarb``
dependency.
"""

from .session import Session

__version__ = "0.3.0"
__all__ = ["Session"]
