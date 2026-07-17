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

import quarb

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
    got = quarb.run('/books/*[/price:: > 20]/title::', BOOKS_JSON, "json")
    assert got == ["Sapiens"]


def test_json_nodes_render_as_pointers():
    got = quarb.run("/books/*", BOOKS_JSON, "json")
    assert got == ["/books/0", "/books/1"]


def test_csv_locale_sort():
    doc = "name\nэхо\nарбуз\nЯблоко\nбанан\n"
    got = quarb.run("/row::name @| sort('ru-RU')", doc, "csv")
    # Russian collation: я sorts last, case-insensitively — a
    # bytewise sort would put Яблоко first.
    assert got == ["арбуз", "банан", "эхо", "Яблоко"]


def test_tsv_delimiter():
    doc = "a\tb\n1\t2\n"
    assert quarb.run("/row::b", doc, "tsv") == ["2"]


def test_yaml_values():
    assert quarb.run("/a::", "a: 1\nb: 2\n", "yaml") == ["1"]


def test_markdown_runs():
    assert quarb.run("//text @| count", "# Title\n\nHello.\n", "markdown")


def test_version():
    assert isinstance(quarb.__version__, str)
    assert quarb.__version__


# --------------------------------------------------------------------------
# run_file — extension inference
# --------------------------------------------------------------------------


def test_run_file_str_and_pathlike(tmp_path):
    p = tmp_path / "books.json"
    p.write_text(BOOKS_JSON)
    want = ["Sapiens", "Cosmos"]
    assert quarb.run_file("/books/*/title::", str(p)) == want
    assert quarb.run_file("/books/*/title::", pathlib.Path(p)) == want


def test_run_file_yml(tmp_path):
    p = tmp_path / "doc.yml"
    p.write_text("a: 1\n")
    assert quarb.run_file("/a::", p) == ["1"]


def test_run_file_unknown_extension(tmp_path):
    p = tmp_path / "doc.dat"
    p.write_text("{}")
    with pytest.raises(ValueError, match="cannot infer format"):
        quarb.run_file("/a::", p)


def test_run_file_missing_file(tmp_path):
    with pytest.raises(OSError):
        quarb.run_file("/a::", tmp_path / "absent.json")


# --------------------------------------------------------------------------
# Errors surface as ValueError with the engine's message
# --------------------------------------------------------------------------


def test_malformed_input():
    with pytest.raises(ValueError, match="parsing JSON"):
        quarb.run("/a::", '{"a": ', "json")


def test_bad_query():
    with pytest.raises(ValueError, match="parse error"):
        quarb.run("/[[", "{}", "json")


def test_unknown_format():
    with pytest.raises(ValueError, match="unknown format"):
        quarb.run("/a::", "{}", "exe")


# --------------------------------------------------------------------------
# Document — the pythonic layer
# --------------------------------------------------------------------------


def test_document_values_are_typed():
    doc = quarb.loads(BOOKS_JSON, "json")
    assert doc.values("/books/*/price::") == [25, 18]
    assert doc.values("/books/*/title::") == ["Sapiens", "Cosmos"]


def test_document_value_scalar():
    doc = quarb.loads(BOOKS_JSON, "json")
    got = doc.value("/books/*/price:: @| mean")
    assert isinstance(got, float) and got == 21.5


def test_document_value_empty_is_none():
    doc = quarb.loads(BOOKS_JSON, "json")
    assert doc.value("/books/*[/price:: > 999]/title::") is None


def test_document_value_stream_raises():
    doc = quarb.loads(BOOKS_JSON, "json")
    with pytest.raises(ValueError, match="use values"):
        doc.value("/books/*/title::")


def test_document_records_are_dicts():
    doc = quarb.loads(BOOKS_JSON, "json")
    got = doc.records('/books/* | rec("t", /title::, "p", /price::)')
    assert got == [
        {"t": "Sapiens", "p": 25},
        {"t": "Cosmos", "p": 18},
    ]


def test_document_records_rejects_scalars():
    doc = quarb.loads(BOOKS_JSON, "json")
    with pytest.raises(TypeError, match="record"):
        doc.records("/books/*/title::")


def test_document_nodes_and_values_discipline():
    doc = quarb.loads(BOOKS_JSON, "json")
    assert doc.nodes("/books/*") == ["/books/0", "/books/1"]
    with pytest.raises(TypeError, match="values"):
        doc.nodes("/books/*/title::")


def test_document_null_is_none():
    doc = quarb.loads('{"a": null}', "json")
    assert doc.values("/a::") == [None]


def test_document_instant_is_aware_datetime():
    import datetime

    doc = quarb.loads('{"t": "2026-07-12T09:30:00Z"}', "json")
    got = doc.value("/t:: | datetime")
    assert isinstance(got, datetime.datetime)
    assert got.tzinfo is not None
    assert got.astimezone(datetime.timezone.utc).hour == 9


def test_document_duration_is_timedelta():
    import datetime

    doc = quarb.loads(
        '{"a": "2026-07-12T09:00:00Z", "b": "2026-07-11T09:00:00Z"}',
        "json",
    )
    got = doc.value("/a:: | datetime | $_ - (^/b:: | datetime)")
    assert got == datetime.timedelta(days=1)


def test_document_reuse_and_format():
    doc = quarb.loads("a,b\n1,2\n3,4\n", "csv")
    assert doc.format == "csv"
    # CSV cells are text; the engine's readings coerce at use.
    assert doc.values("/row::a") == ["1", "3"]
    assert doc.value("/row::b @| sum") == 6.0


def test_load_infers_format(tmp_path):
    p = tmp_path / "conf.toml"
    p.write_text('x = 5\n', encoding="utf-8")
    assert quarb.load(p).value("/x::") == 5


def test_repr():
    doc = quarb.loads("{}", "json")
    assert repr(doc) == "<quarb.Document format='json'>"
