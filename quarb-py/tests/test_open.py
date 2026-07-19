"""quarb.open() dispatch and translate() — self-contained fixtures."""
import sqlite3
import quarb


def test_sqlite_fk_walk(tmp_path):
    db = tmp_path / "shop.db"
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
    d = quarb.open(str(db))
    assert d.values("/tracks/1::artist_id~>::name") == ["Holst"]


def test_kaiv_units(tmp_path):
    doc = tmp_path / "lab.kaiv"
    doc.write_text(".!kaiv 1\n\n!float:W\n/host::draw=142.5\n")
    d = quarb.open(str(doc))
    (q,) = d.values("/host::draw")
    assert (q.magnitude, q.written) == (142.5, "142.5 W")
    assert d.values("/host[::draw > 0.2kW]::draw") == []
    assert len(d.values("/host[::draw > 0.1kW]::draw")) == 1


def test_fs_and_code(tmp_path):
    (tmp_path / "a.py").write_text("def one():\n    return 1\n\ndef two():\n    return 2\n")
    (tmp_path / "note.txt").write_text("x" * 2048)
    d = quarb.open(str(tmp_path))
    assert d.values("//note.txt[::;size > 1kB]:::name") == ["note.txt"]
    c = quarb.open(str(tmp_path / "a.py"))
    assert c.values("//function_definition @| count") == [2]


def test_fs_descend_grafts(tmp_path):
    (tmp_path / "cfg.json").write_text('{"port": 8080}')
    d = quarb.open(str(tmp_path), descend=True)
    assert d.values("/cfg.json/port::") == [8080]


def test_translate():
    assert quarb.translate(".users[].name", "jq")
    assert quarb.translate("SELECT name FROM t WHERE age > 30", "sql")
    assert quarb.translate("//book/title", "xpath")


def test_mount_cross_source_join(tmp_path):
    import sqlite3
    import quarb

    (tmp_path / "fleet.yaml").write_text(
        "hosts:\n  - name: web-01\n    role: web\n  - name: db-01\n    role: db\n"
    )
    db = tmp_path / "cmdb.db"
    con = sqlite3.connect(db)
    con.executescript(
        "CREATE TABLE hosts(name TEXT, owner TEXT);"
        "INSERT INTO hosts VALUES('web-01','ops'),('db-01','dba');"
    )
    con.commit(); con.close()
    d = quarb.mount([str(tmp_path / "fleet.yaml"), str(db)])
    assert d.values("/*") == ["/fleet", "/cmdb"]
    rows = d.records(
        "/fleet/hosts/* <=> /cmdb/hosts/*[::name = $*1/name::] "
        "| rec('host', $*1/name::, 'owner', ::owner)"
    )
    assert rows == [
        {"host": "web-01", "owner": "ops"},
        {"host": "db-01", "owner": "dba"},
    ]
    # A single-path mount() just opens (keeps typed rendering).
    single = quarb.mount([str(db)])
    assert single.values("/hosts/1::owner") == ["ops"]
    # Colliding stems are refused.
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "cmdb.db").write_bytes((db).read_bytes())
    try:
        quarb.mount([str(db), str(tmp_path / "sub" / "cmdb.db")])
        assert False, "expected a stem-collision error"
    except ValueError as e:
        assert "colliding" in str(e)
