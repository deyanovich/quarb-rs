"""The Quarb kernel end-to-end (jupyter_client) and the resident path."""
import json
import queue
import sqlite3

import pytest

pytest.importorskip("ipykernel")
pytest.importorskip("jupyter_client")


@pytest.fixture(scope="module")
def kernel(tmp_path_factory):
    """A live Quarb kernel via a manually-written kernelspec."""
    from jupyter_client.manager import KernelManager

    d = tmp_path_factory.mktemp("spec") / "quarb"
    d.mkdir()
    import sys

    (d / "kernel.json").write_text(json.dumps({
        "argv": [sys.executable, "-m", "quarb.kernel", "-f",
                 "{connection_file}"],
        "display_name": "Quarb", "language": "quarb",
    }))
    km = KernelManager(kernel_name="quarb")
    km.kernel_spec_manager.kernel_dirs.insert(0, str(d.parent))
    km.start_kernel()
    kc = km.client()
    kc.start_channels()
    kc.wait_for_ready(timeout=30)
    yield kc
    kc.stop_channels()
    km.shutdown_kernel(now=True)


def run_cell(kc, code, timeout=30):
    msg_id = kc.execute(code)
    outputs = []
    status = None
    while True:
        try:
            msg = kc.get_iopub_msg(timeout=timeout)
        except queue.Empty:
            break
        if msg["parent_header"].get("msg_id") != msg_id:
            continue
        t = msg["msg_type"]
        if t in ("stream", "execute_result", "error"):
            outputs.append(msg)
        if t == "status" and msg["content"]["execution_state"] == "idle":
            break
    reply = kc.get_shell_msg(timeout=timeout)
    status = reply["content"]["status"]
    return status, outputs


def test_kernel_mount_and_query(kernel, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("kdata")
    db = tmp / "shop.db"
    con = sqlite3.connect(db)
    con.executescript(
        "CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT);"
        "INSERT INTO artists VALUES (1, 'Holst');"
    )
    con.commit(); con.close()
    status, out = run_cell(kernel, f"%mount {db}\n/artists/1::name")
    assert status == "ok"
    result = [o for o in out if o["msg_type"] == "execute_result"]
    assert result and "Holst" in result[0]["content"]["data"]["text/plain"]


def test_kernel_directives_and_errors(kernel):
    status, out = run_cell(kernel, "%docs")
    assert status == "ok"
    status, out = run_cell(kernel, "%bogus")
    assert status == "error"
    # The kernel survives errors: query again.
    status, _ = run_cell(kernel, "%docs")
    assert status == "ok"


def test_kernel_completion(kernel, tmp_path_factory):
    import json as _json
    tmp = tmp_path_factory.mktemp("kc")
    (tmp / "d.json").write_text('{"servers":[{"host":"a"}],"config":{}}')
    run_cell(kernel, f"%mount {tmp / 'd.json'}")
    reply = kernel.complete("/serv", 5)
    msg = kernel.get_shell_msg(timeout=15)
    assert msg["content"]["matches"] == ["servers"]
