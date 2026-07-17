fn main() {
    let path = std::env::args().nth(1).unwrap();
    let _ = std::fs::remove_file(&path);
    let conn = duckdb::Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
      CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT, country TEXT);
      CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, secs INTEGER,
                           price DOUBLE, artist_id INTEGER REFERENCES artists(id));
      INSERT INTO artists VALUES (1,'Holst','GB'), (2,'Bartok','HU');
      INSERT INTO tracks VALUES (1,'Mars',430,1.29,1),(2,'Venus',480,0.99,1),
        (3,'Bourree',95,0.99,2);
      COPY tracks TO '/tmp/tracks.parquet' (FORMAT PARQUET);
    "#,
    )
    .unwrap();
    println!("duck fixture ok");
}
