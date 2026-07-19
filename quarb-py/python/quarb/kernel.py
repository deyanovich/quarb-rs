"""The Quarb Jupyter kernel: every cell is a query.

Install the kernelspec once::

    python -m quarb.kernel install        # --user by default

then pick **Quarb** in the Jupyter launcher. Cells are Quarb
queries against the session's mounts; a small directive family
(lines starting with ``%``) manages state:

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
            "mimetype": "text/plain",
            "file_extension": ".quarb",
        }
        banner = BANNER

        def __init__(self, **kwargs):
            super().__init__(**kwargs)
            self.session = Session()

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
                return self.session.mount(rest)
            if word == "connect":
                return self.session.connect(rest)
            if word == "use":
                self.session._pick(rest)  # validates
                self.session.default = rest
                return f"default: {rest}"
            if word == "docs":
                pool = sorted([*self.session.docs, *self.session.resident])
                return ", ".join(pool) or "(nothing mounted)"
            if word == "translate":
                lang, _, src = rest.partition(" ")
                return translate(src.strip(), lang)
            raise ValueError(
                f"unknown directive %{word} "
                "(mount, connect, use, docs, translate)"
            )

        # -- protocol --------------------------------------------
        def do_execute(
            self, code, silent, store_history=True,
            user_expressions=None, allow_stdin=False, *, cell_meta=None,
            cell_id=None,
        ):
            try:
                lines = [l for l in code.strip().splitlines() if l.strip()]
                directives = [l for l in lines if l.lstrip().startswith("%")]
                query_lines = [l for l in lines if not l.lstrip().startswith("%")]
                for d in directives:
                    out = self._directive(d.strip())
                    if not silent and out:
                        self._send_text(out + "\n")
                if query_lines:
                    result = self.session.run("\n".join(query_lines))
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

    return QuarbKernel


KERNEL_JSON = {
    "argv": [sys.executable, "-m", "quarb.kernel", "-f", "{connection_file}"],
    "display_name": "Quarb",
    "language": "quarb",
}


def install(user: bool = True):
    """Write the kernelspec so Quarb appears in the launcher."""
    import json
    import tempfile
    from pathlib import Path

    from jupyter_client.kernelspec import KernelSpecManager

    with tempfile.TemporaryDirectory() as td:
        d = Path(td) / "quarb"
        d.mkdir()
        (d / "kernel.json").write_text(json.dumps(KERNEL_JSON, indent=2))
        path = KernelSpecManager().install_kernel_spec(
            str(d), "quarb", user=user
        )
    print(f"installed kernelspec: {path}")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "install":
        install(user="--sys-prefix" not in sys.argv)
        return
    from ipykernel.kernelapp import IPKernelApp

    IPKernelApp.launch_instance(kernel_class=_make_kernel_class())


if __name__ == "__main__":
    main()
