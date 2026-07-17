//! `xpath2quarb` — translate an XPath 1.0 expression to a Quarb
//! query. The query goes to stdout; semantic-divergence notes go to
//! stderr.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(xpath), None) = (args.next(), args.next()) else {
        eprintln!("usage: xpath2quarb <XPATH>");
        return ExitCode::from(2);
    };
    match quarb_xpath::translate(&xpath) {
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
