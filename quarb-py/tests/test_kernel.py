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


def test_kernel_man_is_a_cell(kernel):
    # `man sort` as a cell: the manual, as `%docs sort` is; a glyph
    # and the list of glyphs the same
    status, out = run_cell(kernel, "man upper")
    assert status == "ok"
    text = "".join(o["content"]["text"] for o in out if o["msg_type"] == "stream")
    assert text.startswith("upper — the text in upper case")
    status, out = run_cell(kernel, "man <=>")
    assert status == "ok"
    text = "".join(o["content"]["text"] for o in out if o["msg_type"] == "stream")
    assert text.startswith("<=> — correlation")
    status, out = run_cell(kernel, "%docs upper")
    assert status == "ok"
    # a word the manual lacks is the cell's error
    status, _ = run_cell(kernel, "man qqqq")
    assert status == "error"


def test_kernel_cross_cell_defs(kernel, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("kdefs")
    (tmp / "crew.json").write_text(
        '{"crew": [{"name": "Ada", "born": "1881-12-25"}]}'
    )
    run_cell(kernel, f"%mount {tmp / 'crew.json'}")
    # A defs-only cell extends the fragment table, with comments.
    status, out = run_cell(
        kernel,
        "# Julian to Gregorian, 1900-1917\n"
        "def &gregorian: | tp('%Y-%m-%d') | $_ + '12d';",
    )
    assert status == "ok"
    streams = [o for o in out if o["msg_type"] == "stream"]
    assert streams and "&gregorian" in streams[0]["content"]["text"]
    # A later cell resolves the name.
    convert = '/crew/* | %(name = ::name; g = (::born | &gregorian))'
    status, out = run_cell(kernel, convert)
    assert status == "ok"
    result = [o for o in out if o["msg_type"] == "execute_result"]
    # The fragment's tp() yields a typed instant: a datetime repr.
    assert result and "1882, 1, 6" in result[0]["content"]["data"]["text/plain"]
    # A malformed defs cell leaves the table untouched.
    status, _ = run_cell(kernel, "def &broken: ;")
    assert status == "error"
    status, out = run_cell(kernel, convert)
    assert status == "ok"


def test_kernel_previous_line(kernel, tmp_path_factory):
    """`&_` is the last query cell that ran, `&-N` the N-th back,
    `&N` the cell of `In [N]` (ruling 2026-11-11): a cell that ran
    takes its execution count as its label."""
    tmp = tmp_path_factory.mktemp("kprev")
    (tmp / "books.json").write_text(
        '{"books": [{"title": "Emma"}, {"title": "Iliad"}]}'
    )
    run_cell(kernel, f"%mount {tmp / 'books.json'}")

    def plain(code):
        status, out = run_cell(kernel, code)
        result = [o for o in out if o["msg_type"] == "execute_result"]
        text = result[0]["content"]["data"]["text/plain"] if result else ""
        count = result[0]["content"]["execution_count"] if result else None
        errors = [o["content"]["evalue"] for o in out if o["msg_type"] == "error"]
        return status, text, count, errors

    status, text, titles_at, _ = plain("/books/*::title")
    assert status == "ok" and "Emma" in text and "Iliad" in text
    # the line before this one, with a tail
    status, text, _, _ = plain("&_ @| count")
    assert status == "ok" and text.strip() == "2", text
    # a cell that fails takes no label: `&_` is still the count
    status, _, _, _ = plain("/books/*::titel |")
    assert status == "error"
    status, text, _, _ = plain("&_")
    assert status == "ok" and text.strip() == "2", text
    # three labelled cells back: the titles; and by its number
    status, text, _, _ = plain("&-3 | upper")
    assert status == "ok" and "EMMA" in text, text
    status, text, _, _ = plain(f"&{titles_at} | upper")
    assert status == "ok" and "ILIAD" in text, text
    # the rounded twin
    status, text, _, _ = plain("(=_) @| count")
    assert status == "ok" and text.strip() == "2", text
    # past the first labelled cell
    status, _, _, errors = plain("&-99")
    assert status == "error"
    assert errors and "'&-99': no line 99 back" in errors[0], errors


def test_session_labels_and_previous():
    """The session's own: labels oldest first, a query that cannot
    be a fragment's body takes none."""
    from quarb.session import Session

    s = Session()
    with pytest.raises(ValueError, match="no line before this one"):
        s.resolve_previous("&_ | count")
    assert s.remember(3, "/books/*::title")
    assert s.remember(7, "&3 @| count")
    assert not s.remember(7, "/again"), "a label is past the last one"
    assert not s.remember(9, "def &x: /a ; /b"), "inline definitions"
    assert s.labels == [3, 7]
    assert s.resolve_previous("&_ | upper") == "&7 | upper"
    assert s.resolve_previous("&-2") == "&3"
    assert s.resolve_previous("(=_)") == "(=7)"
    assert s.resolve_previous('= "&_"') == '= "&_"'
    assert "def &3: /books/*::title ;" in s.history_text


def test_kernel_completion(kernel, tmp_path_factory):
    import json as _json
    tmp = tmp_path_factory.mktemp("kc")
    (tmp / "d.json").write_text('{"servers":[{"host":"a"}],"config":{}}')
    run_cell(kernel, f"%mount {tmp / 'd.json'}")
    reply = kernel.complete("/serv", 5)
    msg = kernel.get_shell_msg(timeout=15)
    assert msg["content"]["matches"] == ["servers"]
