"""Behavioral tests for the qua Python binding.

Exercises the PyO3 boundary the Rust engine tests cannot reach:
format dispatch across the text adapters, node vs. value rendering,
extension inference in ``run_file`` (str and PathLike), and the
error-to-``ValueError`` mapping.

Run with the wheel installed (e.g. ``maturin develop`` then
``pytest``).
"""

import pathlib

import pytest

import qua

BOOKS_JSON = """{
  "books": [
    {"title": "Sapiens", "price": 25},
    {"title": "Cosmos",  "price": 18}
  ]
}"""


# --------------------------------------------------------------------------
# run — dispatch and rendering
# --------------------------------------------------------------------------


def test_json_values():
    got = qua.run('/books/*[/price:: > 20]/title::', BOOKS_JSON, "json")
    assert got == ["Sapiens"]


def test_json_nodes_render_as_pointers():
    got = qua.run("/books/*", BOOKS_JSON, "json")
    assert got == ["/books/0", "/books/1"]


def test_csv_locale_sort():
    doc = "name\nэхо\nарбуз\nЯблоко\nбанан\n"
    got = qua.run("/row::name @| sort('ru-RU')", doc, "csv")
    # Russian collation: я sorts last, case-insensitively — a
    # bytewise sort would put Яблоко first.
    assert got == ["арбуз", "банан", "эхо", "Яблоко"]


def test_tsv_delimiter():
    doc = "a\tb\n1\t2\n"
    assert qua.run("/row::b", doc, "tsv") == ["2"]


def test_yaml_values():
    assert qua.run("/a::", "a: 1\nb: 2\n", "yaml") == ["1"]


def test_markdown_runs():
    assert qua.run("//text @| count", "# Title\n\nHello.\n", "markdown")


def test_version():
    assert isinstance(qua.__version__, str)
    assert qua.__version__


# --------------------------------------------------------------------------
# run_file — extension inference
# --------------------------------------------------------------------------


def test_run_file_str_and_pathlike(tmp_path):
    p = tmp_path / "books.json"
    p.write_text(BOOKS_JSON)
    want = ["Sapiens", "Cosmos"]
    assert qua.run_file("/books/*/title::", str(p)) == want
    assert qua.run_file("/books/*/title::", pathlib.Path(p)) == want


def test_run_file_yml(tmp_path):
    p = tmp_path / "doc.yml"
    p.write_text("a: 1\n")
    assert qua.run_file("/a::", p) == ["1"]


def test_run_file_unknown_extension(tmp_path):
    p = tmp_path / "doc.dat"
    p.write_text("{}")
    with pytest.raises(ValueError, match="cannot infer format"):
        qua.run_file("/a::", p)


def test_run_file_missing_file(tmp_path):
    with pytest.raises(OSError):
        qua.run_file("/a::", tmp_path / "absent.json")


# --------------------------------------------------------------------------
# Errors surface as ValueError with the engine's message
# --------------------------------------------------------------------------


def test_malformed_input():
    with pytest.raises(ValueError, match="parsing JSON"):
        qua.run("/a::", '{"a": ', "json")


def test_bad_query():
    with pytest.raises(ValueError, match="parse error"):
        qua.run("/[[", "{}", "json")


def test_unknown_format():
    with pytest.raises(ValueError, match="unknown format"):
        qua.run("/a::", "{}", "exe")
