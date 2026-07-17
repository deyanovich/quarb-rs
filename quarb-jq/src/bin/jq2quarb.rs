//! `jq2quarb` — translate a jq filter to a Quarb query. The query
//! goes to stdout; semantic-divergence notes go to stderr.
//!
//! By default the translation projects values (matching jq's value
//! output); `--nodes` leaves the result as a node query instead,
//! comparable with `jq 'path(...)'`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut nodes = false;
    let mut filter = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--nodes" => nodes = true,
            _ if filter.is_none() => filter = Some(arg),
            _ => {
                eprintln!("usage: jq2quarb [--nodes] <FILTER>");
                return ExitCode::from(2);
            }
        }
    }
    let Some(filter) = filter else {
        eprintln!("usage: jq2quarb [--nodes] <FILTER>");
        return ExitCode::from(2);
    };
    let result = if nodes {
        quarb_jq::translate_nodes(&filter)
    } else {
        quarb_jq::translate(&filter)
    };
    match result {
        Ok(tr) => {
            for note in &tr.notes {
                eprintln!("note: {note}");
            }
            println!("{}", tr.query);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
