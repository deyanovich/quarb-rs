//! Translate a SQL SELECT statement (argv or stdin) to a Quarb
//! query. Notes go to stderr, the query to stdout.
//!
//! `--lenient` is taken, as the other translators take it, and
//! changes nothing yet: what this bridge refuses has no near Quarb
//! construct to fall back to (or translates exactly, and is not
//! written).

use std::io::Read;

fn main() {
    // the translators' common switch: no lenient form here yet
    let lenient = std::env::args().skip(1).any(|a| a == "--lenient");
    let input = match std::env::args().skip(1).find(|a| a != "--lenient") {
        Some(a) => a,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).expect("stdin");
            s
        }
    };
    match quarb_sql::translate(&input) {
        Ok(t) => {
            for note in &t.notes {
                eprintln!("note: {note}");
            }
            println!("{}", t.query);
        }
        // a worded refusal is the line `qua` prints for it
        Err(e) => {
            match e.not_translated() {
                Some((code, reason, takes)) => eprintln!(
                    "{}",
                    quarb::diagnostic::translator_refusal("sql", code, reason, takes, lenient)
                ),
                None => eprintln!("error: {e}"),
            }
            std::process::exit(1);
        }
    }
}
