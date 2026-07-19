"""The %quarb magics, driven through a real InteractiveShell."""
import sqlite3

import pytest

ipython = pytest.importorskip("IPython")
from IPython.testing.globalipapp import get_ipython  # noqa: E402


@pytest.fixture(scope="module")
def shell():
    sh = get_ipython()
    sh.run_line_magic("load_ext", "quarb.ipython")
    return sh


def test_mount_and_query(shell, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("nb")
    db = tmp / "shop.db"
    con = sqlite3.connect(db)
    con.executescript(
        """
        CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT,
                             artist_id INTEGER REFERENCES artists(id));
        INSERT INTO artists VALUES (1, 'Holst');
        INSERT INTO tracks VALUES (1, 'Jupiter', 1);
        """
    )
    con.commit()
    con.close()
    shell.run_line_magic("quarb_mount", str(db))
    r = shell.run_line_magic("quarb", "/tracks/1::artist_id~>::name")
    assert r.values == ["Holst"]


def test_cell_magic_records_and_df(shell, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("nb2")
    lab = tmp / "lab.kaiv"
    lab.write_text(
        ".!kaiv 1\n\n[/@hosts]\nname=web-01\n!float:W\ndraw=142.5\n[]\n\n"
        "[/@hosts]\nname=db-01\n!float:W\ndraw=290\n[]\n"
    )
    shell.run_line_magic("quarb_mount", str(lab))
    r = shell.run_cell_magic(
        "quarb", "lab", '/@hosts/* | rec("name", /name::, "draw", ::draw)'
    )
    assert [rec["name"] for rec in r.records] == ["web-01", "db-01"]
    assert "table" in r._repr_html_()
    pd = pytest.importorskip("pandas")
    df = r.df_magnitudes()
    assert list(df["draw"]) == [142.5, 290.0]
    # Typed objects survive in the plain frame.
    assert str(r.df["draw"][0]) == "142.5 W"


def test_named_documents(shell, tmp_path_factory):
    tmp = tmp_path_factory.mktemp("nb3")
    (tmp / "a.json").write_text('{"x": 1}')
    (tmp / "b.json").write_text('{"x": 2}')
    # Separate mounts keep separate named documents (a single
    # multi-path mount would instead join them under one root).
    shell.run_line_magic("quarb_mount", f"{tmp}/a.json")
    shell.run_line_magic("quarb_mount", f"{tmp}/b.json")
    ra = shell.run_cell_magic("quarb", "a", "/x::")
    rb = shell.run_cell_magic("quarb", "b", "/x::")
    assert (ra.values, rb.values) == ([1], [2])


def test_completion_from_live_data(tmp_path):
    import quarb
    from quarb.session import Session

    p = tmp_path / "d.json"
    p.write_text('{"users":[{"name":"ada","addr":{"city":"x"}}],"meta":{"v":1}}')
    s = Session()
    s.mount(str(p))
    assert s.complete("/us", 3)[0] == ["users"]
    assert s.complete("/users/*/na", 11)[0] == ["name"]
    assert set(s.complete("/", 1)[0]) == {"meta", "users"}
    # Predicate-relative: the path inside [ ... ] hangs on the node.
    assert s.complete("/users/*[/na", 12)[0] == ["name"]
    assert s.complete("/users/*[/addr/ci", 17)[0] == ["city"]
    # No completion inside a literal or with no mount.
    assert s.complete('/users/*[::name = "a', 20)[0] == []
    assert Session().complete("/x", 2)[0] == []


def test_session_multimount_joins(tmp_path):
    import sqlite3
    from quarb.session import Session

    (tmp_path / "a.yaml").write_text("rows:\n  - k: x\n  - k: y\n")
    db = tmp_path / "b.db"
    con = sqlite3.connect(db)
    con.executescript("CREATE TABLE t(k TEXT, v INTEGER);"
                      "INSERT INTO t VALUES('x',1),('y',2);")
    con.commit(); con.close()
    s = Session()
    msg = s.mount(f"{tmp_path / 'a.yaml'} {db}")
    assert "a+b" in msg and "/a" in msg and "/b" in msg
    r = s.run("/a/rows/* <=> /b/t/*[::k = $*1/k::] | rec('k', $*1/k::, 'v', ::v)")
    assert r.records == [{"k": "x", "v": 1}, {"k": "y", "v": 2}]
