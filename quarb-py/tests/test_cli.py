"""The ``qua`` console script of the base package (quarb._cli).

It reads its input as the full CLI does: standard input is JSON
unless ``--format`` names another notation, and a path opens as
``quarb.open`` opens it.
"""
import io
import sqlite3

import pytest

from quarb import _cli


def run(argv, capsys, monkeypatch, stdin=None):
    if stdin is not None:
        monkeypatch.setattr("sys.stdin", io.StringIO(stdin))
    code = _cli.main(argv)
    out = capsys.readouterr()
    return code, out.out, out.err


def test_stdin_is_json_by_default(capsys, monkeypatch):
    code, out, err = run(
        ["/books/*/title::"],
        capsys,
        monkeypatch,
        stdin='{"books": [{"title": "Emma"}, {"title": "Iliad"}]}',
    )
    assert (code, out, err) == (0, "Emma\nIliad\n", "")


def test_stdin_format_and_its_direct_flag(capsys, monkeypatch):
    text = "books:\n  - title: Emma\n  - title: Iliad\n"
    for flags in (["--format", "yaml"], ["-f", "yaml"], ["--yaml"]):
        code, out, err = run(
            ["/books/*/title::", *flags], capsys, monkeypatch, stdin=text
        )
        assert (code, out, err) == (0, "Emma\nIliad\n", ""), flags


def test_stdin_not_json_is_refused_not_sniffed(capsys, monkeypatch):
    code, out, err = run(["/a::"], capsys, monkeypatch, stdin="a: 1\n")
    assert code == 1 and out == ""
    assert err.startswith("qua: parsing JSON")


def test_a_text_file_by_its_extension(tmp_path, capsys, monkeypatch):
    p = tmp_path / "books.json"
    p.write_text('{"books": [{"title": "Emma"}]}')
    # node results keep the text formats' own rendering (the path,
    # a list item named from one)
    code, out, err = run(["/books/*", str(p)], capsys, monkeypatch)
    assert (code, out, err) == (0, "/books/1\n", "")


def test_a_file_under_another_format(tmp_path, capsys, monkeypatch):
    p = tmp_path / "books.dat"
    p.write_text('{"books": [{"title": "Emma"}]}')
    code, out, err = run(
        ["/books/*/title::", str(p), "--format", "json"], capsys, monkeypatch
    )
    assert (code, out, err) == (0, "Emma\n", "")


def test_a_path_opens_as_open_opens_it(tmp_path, capsys, monkeypatch):
    # a SQLite database: no text format by that extension
    db = tmp_path / "library.db"
    con = sqlite3.connect(db)
    con.executescript(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT);"
        "INSERT INTO books VALUES (1, 'Emma'), (2, 'Iliad');"
    )
    con.commit()
    con.close()
    code, out, err = run(["/books/* | ::title", str(db)], capsys, monkeypatch)
    assert (code, out, err) == (0, "Emma\nIliad\n", "")
    # a directory
    (tmp_path / "notes").mkdir()
    (tmp_path / "notes" / "a.txt").write_text("x")
    code, out, err = run(["/* | :name", str(tmp_path / "notes")], capsys, monkeypatch)
    assert (code, out, err) == (0, "a.txt\n", "")


def test_a_missing_file_is_refused(tmp_path, capsys, monkeypatch):
    for name in ("absent.json", "absent.db"):
        code, out, err = run(["/a::", str(tmp_path / name)], capsys, monkeypatch)
        assert code == 1 and out == "", name
        assert err.startswith("qua: "), name


def test_a_bad_query_is_refused(capsys, monkeypatch):
    code, out, err = run(["/a["], capsys, monkeypatch, stdin="{}")
    assert code == 1 and out == ""
    assert err.startswith("qua: parse error")


def test_unknown_format_is_a_usage_error(capsys, monkeypatch):
    with pytest.raises(SystemExit) as e:
        _cli.main(["/a::", "--format", "nosuch"])
    assert e.value.code == 2
