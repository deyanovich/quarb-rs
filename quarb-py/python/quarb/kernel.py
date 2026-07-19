"""The Quarb Jupyter kernel: every cell is a query.

Install the kernelspec once::

    python -m quarb.kernel install        # --user by default

then pick **Quarb** in the Jupyter launcher. Cells are Quarb
queries against the session's mounts; a small directive family
(lines starting with ``%``) manages state:

- ``%python`` — the rest of the cell is Python (build a fixture,
  post-process, plot); the namespace persists, ``session`` is the
  live session, and ``_`` is the last query result.
- ``%mount PATH [PATH ...] [--descend]`` — open documents
  in-process (directories, SQLite, kaiv, ``git:PATH``, archives,
  XLSX, source files, text formats).
- ``%connect NAME PATH [PATH ...]`` — a resident target set
  served by ``qua --resident`` (for arbors too large to rebuild
  per notebook; needs the qua binary on PATH).
- ``%use NAME`` — switch the default document.
- ``%docs`` — list what's mounted.
- ``%translate LANG QUERY`` — show a jq / xpath / sql query as
  Quarb text.

Record streams render as HTML tables; everything else as plain
lines. The wrapper delegates all protocol plumbing to ipykernel
(``pip install quarb[jupyter]``).
"""

from __future__ import annotations

import sys
from pathlib import Path

from . import translate
from .session import Session

BANNER = "Quarb — one query language for every tree (quarb.org)"


def _make_kernel_class():
    from ipykernel.kernelbase import Kernel

    class QuarbKernel(Kernel):
        implementation = "quarb"
        implementation_version = "0.3"
        language = "quarb"
        language_version = "0.3"
        language_info = {
            "name": "quarb",
            # The MIME and codemirror_mode the quarb-lab JupyterLab
            # extension registers its highlighting under; without the
            # extension these degrade to plain text.
            "mimetype": "text/x-quarb",
            "codemirror_mode": "quarb",
            "file_extension": ".quarb",
        }
        banner = BANNER

        def __init__(self, **kwargs):
            super().__init__(**kwargs)
            self.qsession = Session()
            # A persistent namespace for %python escapes — build a
            # fixture, post-process a result, plot. The last query
            # result is bound to `_`.
            import quarb as _quarb

            self.pyns = {"quarb": _quarb, "session": self.qsession, "_": None}

        # -- helpers ---------------------------------------------
        def _send_text(self, text: str):
            self.send_response(
                self.iopub_socket, "stream", {"name": "stdout", "text": text}
            )

        def _send_result(self, result):
            data = {"text/plain": repr(result)}
            html = result._repr_html_()
            if html:
                data["text/html"] = html
            self.send_response(
                self.iopub_socket,
                "execute_result",
                {"execution_count": self.execution_count, "data": data,
                 "metadata": {}},
            )

        def _directive(self, line: str) -> str:
            word, _, rest = line[1:].partition(" ")
            rest = rest.strip()
            if word == "mount":
                return self.qsession.mount(rest)
            if word == "connect":
                return self.qsession.connect(rest)
            if word == "use":
                self.qsession._pick(rest)  # validates
                self.qsession.default = rest
                return f"default: {rest}"
            if word == "docs":
                pool = sorted([*self.qsession.docs, *self.qsession.resident])
                return ", ".join(pool) or "(nothing mounted)"
            if word == "translate":
                lang, _, src = rest.partition(" ")
                return translate(src.strip(), lang)
            raise ValueError(
                f"unknown directive %{word} "
                "(python, mount, connect, use, docs, translate)"
            )

        # -- protocol --------------------------------------------
        def do_execute(
            self, code, silent, store_history=True,
            user_expressions=None, allow_stdin=False, *, cell_meta=None,
            cell_id=None,
        ):
            try:
                stripped = code.strip()
                # A cell opening with %python is Python all the way
                # down — an escape hatch for fixtures, post-
                # processing, plotting; the namespace persists.
                if stripped.split("\n", 1)[0].strip() in ("%python", "%%python"):
                    body = stripped.split("\n", 1)[1] if "\n" in stripped else ""
                    import io
                    import contextlib

                    buf = io.StringIO()
                    with contextlib.redirect_stdout(buf):
                        exec(body, self.pyns)  # noqa: S102 — user's own cell
                    if not silent and buf.getvalue():
                        self._send_text(buf.getvalue())
                    return {
                        "status": "ok",
                        "execution_count": self.execution_count,
                        "payload": [], "user_expressions": {},
                    }
                lines = [l for l in stripped.splitlines() if l.strip()]
                directives = [l for l in lines if l.lstrip().startswith("%")]
                query_lines = [l for l in lines if not l.lstrip().startswith("%")]
                for d in directives:
                    out = self._directive(d.strip())
                    if not silent and out:
                        self._send_text(out + "\n")
                if query_lines:
                    result = self.qsession.run("\n".join(query_lines))
                    self.pyns["_"] = result
                    if not silent:
                        self._send_result(result)
            except Exception as e:  # noqa: BLE001 — kernel must not die
                if not silent:
                    self.send_response(
                        self.iopub_socket,
                        "error",
                        {"ename": type(e).__name__, "evalue": str(e),
                         "traceback": [f"{type(e).__name__}: {e}"]},
                    )
                return {
                    "status": "error", "execution_count": self.execution_count,
                    "ename": type(e).__name__, "evalue": str(e),
                    "traceback": [],
                }
            return {
                "status": "ok", "execution_count": self.execution_count,
                "payload": [], "user_expressions": {},
            }

        def do_complete(self, code, cursor_pos):
            matches, start, end = self.qsession.complete(code, cursor_pos)
            return {
                "status": "ok", "matches": matches,
                "cursor_start": start, "cursor_end": end, "metadata": {},
            }

    return QuarbKernel


KERNEL_JSON = {
    "argv": [sys.executable, "-m", "quarb.kernel", "-f", "{connection_file}"],
    "display_name": "Quarb",
    "language": "quarb",
}


def install(user: bool = False, prefix: str | None = None):
    """Write the kernelspec so Quarb appears in the launcher.

    Defaults to the running environment's prefix (the venv or
    conda env), which is what you want inside one; pass user=True
    for ~/.local, or an explicit prefix.
    """
    import json
    import sys
    import tempfile
    from pathlib import Path

    from jupyter_client.kernelspec import KernelSpecManager

    if not user and prefix is None:
        prefix = sys.prefix
    with tempfile.TemporaryDirectory() as td:
        d = Path(td) / "quarb"
        d.mkdir()
        (d / "kernel.json").write_text(json.dumps(KERNEL_JSON, indent=2))
        path = KernelSpecManager().install_kernel_spec(
            str(d), "quarb", user=user, prefix=prefix
        )
    print(f"installed kernelspec: {path}")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "install":
        install(user="--user" in sys.argv)
        return
    if len(sys.argv) > 1 and sys.argv[1] == "demo":
        import shutil
        from importlib.resources import files

        dest = Path(sys.argv[2] if len(sys.argv) > 2 else ".") / "quarb-demo.ipynb"
        src = files("quarb").joinpath("examples/quarb-demo.ipynb")
        shutil.copyfile(str(src), dest)
        print(f"wrote {dest}")
        return
    from ipykernel.kernelapp import IPKernelApp

    IPKernelApp.launch_instance(kernel_class=_make_kernel_class())


if __name__ == "__main__":
    main()
