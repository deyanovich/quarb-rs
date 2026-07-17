fn main() {
    let path = std::env::args().nth(1).unwrap();
    let conn = duckdb::Connection::open(&path).unwrap();
    let mut stmt2 = conn
        .prepare(
            "SELECT table_name, constraint_column_names, referenced_table, referenced_column_names \
             FROM duckdb_constraints() WHERE constraint_type='FOREIGN KEY'",
        )
        .unwrap();
    let mut rows = stmt2.query([]).unwrap();
    while let Some(r) = rows.next().unwrap() {
        let t: String = r.get(0).unwrap();
        let c: duckdb::types::Value = r.get(1).unwrap();
        let rt: String = r.get(2).unwrap();
        let rc: duckdb::types::Value = r.get(3).unwrap();
        println!("{t} {c:?} -> {rt} {rc:?}");
    }
}
