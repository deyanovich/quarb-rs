//! Translate a Quarb query (argv or stdin) to a SQL SELECT statement. Notes go
//! to stderr, the statement to stdout.

use std::io::Read;

fn main() {
    let input = match std::env::args().nth(1) {
        Some(a) => a,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).expect("stdin");
            s
        }
    };
    match quarb_sql::export(&input) {
        Ok(t) => {
            for note in &t.notes {
                eprintln!("note: {note}");
            }
            println!("{}", t.query);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
