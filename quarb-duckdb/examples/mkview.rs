fn main() {
    let path = std::env::args().nth(1).unwrap();
    let pq = std::env::args().nth(2).unwrap();
    let conn = duckdb::Connection::open(&path).unwrap();
    conn.execute_batch(&format!(
        "CREATE OR REPLACE VIEW pq_tracks AS SELECT * FROM read_parquet('{pq}');"
    ))
    .unwrap();
    println!("view ok");
}
