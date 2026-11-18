//! `jq2quarb` — translate a jq filter to a Quarb query. The query
//! goes to stdout; semantic-divergence notes go to stderr.
//!
//! By default the translation projects values (matching jq's value
//! output); `--nodes` leaves the result as a node query instead,
//! comparable with `jq 'path(...)'`.
//!
//! `--lenient` translates a construct with no exact Quarb
//! counterpart to the nearest one (`sort_by` and the other by-key
//! functions) and names each divergence in a `warning:` line on
//! stderr; without it such a construct is refused, the refusal
//! naming the flag.

use std::process::ExitCode;

fn main() -> ExitCode {
    const USAGE: &str = "usage: jq2quarb [--nodes] [--lenient] <FILTER>";
    let mut nodes = false;
    let mut lenient = false;
    let mut filter = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--nodes" => nodes = true,
            "--lenient" => lenient = true,
            _ if filter.is_none() => filter = Some(arg),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(filter) = filter else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match (nodes, lenient) {
        (false, false) => quarb_jq::translate(&filter),
        (true, false) => quarb_jq::translate_nodes(&filter),
        (false, true) => quarb_jq::translate_lenient(&filter),
        (true, true) => quarb_jq::translate_nodes_lenient(&filter),
    };
    match result {
        Ok(tr) => {
            for warning in &tr.warnings {
                eprintln!("warning: {warning}");
            }
            for note in &tr.notes {
                eprintln!("note: {note}");
            }
            println!("{}", tr.query);
            ExitCode::SUCCESS
        }
        // a worded refusal is the line `qua` prints for it
        Err(e) => {
            match e.not_translated() {
                Some((code, reason, takes)) => eprintln!(
                    "{}",
                    quarb::diagnostic::translator_refusal("jq", code, reason, takes, lenient)
                ),
                None => eprintln!("error: {e}"),
            }
            ExitCode::FAILURE
        }
    }
}
