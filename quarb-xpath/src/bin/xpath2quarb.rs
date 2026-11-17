//! `xpath2quarb` — translate an XPath 1.0 expression to a Quarb
//! query. The query goes to stdout; semantic-divergence notes go to
//! stderr.
//!
//! `--lenient` translates a construct with no exact Quarb
//! counterpart to the nearest one (a relative path, read from the
//! document root; `node()`, the elements) and names each divergence
//! in a `warning:` line on stderr; without it such a construct is
//! refused, the refusal naming the flag.

use std::process::ExitCode;

fn main() -> ExitCode {
    const USAGE: &str = "usage: xpath2quarb [--lenient] <XPATH>";
    let mut lenient = false;
    let mut xpath = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--lenient" => lenient = true,
            _ if xpath.is_none() => xpath = Some(arg),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(xpath) = xpath else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = if lenient {
        quarb_xpath::translate_lenient(&xpath)
    } else {
        quarb_xpath::translate(&xpath)
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
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
