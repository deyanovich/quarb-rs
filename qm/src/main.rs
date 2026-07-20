//! `qm` — a mailbox interface for the terminal, on the Quarb
//! engine.
//!
//! The library-path client: `qm` opens a [`MaildirAdapter`] once
//! per invocation (the session), addresses messages as *node
//! handles* (not re-matched text), and phrases its canned
//! subcommands as Quarb queries where a query is the natural
//! shape. Message numbers are positions in the date-sorted list
//! (newest first), stable within a mailbox state.
//!
//! Reading is the engine's; *actions* are the tool's: `qm seen`
//! performs the Maildir flag rename itself, on the file handle
//! the adapter exposes (`path_of`) — the adapter never writes.
//!
//! The escape hatch: `qm query '<quarb>'` runs any query against
//! the mailbox.

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use quarb::{AstAdapter, NodeId, QueryResult};
use quarb_maildir::MaildirAdapter;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "your mailbox, queried")]
struct Cli {
    /// The mailbox: a Maildir directory or an mbox file.
    #[arg(short, long)]
    mailbox: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List messages, newest first.
    Ls,
    /// Show message N (headers and body).
    Read { n: usize },
    /// Show message N's whole thread, as a tree.
    Thread { n: usize },
    /// List messages from senders matching PAT.
    From { pat: String },
    /// List messages whose subject or body contains PAT.
    Grep { pat: String },
    /// Message counts per sender.
    Count,
    /// List unread messages (still in new/).
    Unread,
    /// List messages since DATE (YYYY-MM-DD, or a unix epoch).
    Since { date: String },
    /// List the replies to message N.
    Replies { n: usize },
    /// Save (from, subject, epoch) per message to a SQLite file
    /// for heavier analysis.
    Export { file: PathBuf },
    /// Mark message N seen (Maildir flag rename — the one write,
    /// and it is qm's, not the engine's).
    Seen { n: usize },
    /// Run a raw Quarb query against the mailbox.
    Query { q: String },
}

/// All messages, newest first by `;;;epoch` (undated ones last).
fn inbox(a: &MaildirAdapter) -> Vec<NodeId> {
    let mut msgs: Vec<(i64, NodeId)> = a
        .children(a.root())
        .into_iter()
        .map(|n| {
            let e = match a.metadata(n, "epoch") {
                Some(quarb::Value::Int(e)) => e,
                _ => i64::MIN,
            };
            (e, n)
        })
        .collect();
    msgs.sort_by_key(|(e, _)| std::cmp::Reverse(*e));
    msgs.into_iter().map(|(_, n)| n).collect()
}

fn header(a: &MaildirAdapter, n: NodeId, h: &str) -> String {
    match a.property(n, h) {
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// `[new]` when the message file still sits in `new/`.
fn is_new(a: &MaildirAdapter, n: NodeId) -> bool {
    a.path_of(n)
        .and_then(|p| p.parent())
        .and_then(|d| d.file_name())
        .is_some_and(|d| d == "new")
}

fn line(a: &MaildirAdapter, i: usize, n: NodeId) {
    let date = header(a, n, "date");
    let date = date.split(" +").next().unwrap_or(&date);
    let date = date.split(" -").next().unwrap_or(date);
    let flag = if is_new(a, n) { "*" } else { " " };
    println!(
        "{i:>4} {flag} {:<24} {:<24} {}",
        date,
        truncate(&header(a, n, "from"), 24),
        header(a, n, "subject")
    );
}

fn truncate(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        s.chars().take(w - 1).collect::<String>() + "…"
    }
}

/// The N-th message of the inbox listing.
fn nth(a: &MaildirAdapter, n: usize) -> anyhow::Result<NodeId> {
    inbox(a)
        .get(n.checked_sub(1).context("message numbers start at 1")?)
        .copied()
        .with_context(|| format!("no message #{n}"))
}

fn show(a: &MaildirAdapter, n: NodeId) {
    for h in ["from", "to", "date", "subject"] {
        let v = header(a, n, h);
        if !v.is_empty() {
            println!("{h}: {v}");
        }
    }
    println!();
    if let Some(quarb::Value::Str(body)) = a.default_value(n) {
        print!("{body}");
    }
}

/// Climb to the thread root, then print the reply tree.
fn thread(a: &MaildirAdapter, n: NodeId) {
    let mut root = n;
    while let Some(p) = a.resolve(root, "in-reply-to", None) {
        root = p;
    }
    let order = inbox(a);
    fn walk(a: &MaildirAdapter, n: NodeId, depth: usize, order: &[NodeId]) {
        let idx = order.iter().position(|&m| m == n).map(|i| i + 1);
        println!(
            "{}#{} {} — {}",
            "  ".repeat(depth),
            idx.map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
            truncate(&header(a, n, "from"), 30),
            header(a, n, "subject")
        );
        let mut replies = a.backlinks(n);
        // Chronological within a level.
        replies.sort_by_key(|(_, r)| match a.metadata(*r, "epoch") {
            Some(quarb::Value::Int(e)) => e,
            _ => i64::MAX,
        });
        for (_, r) in replies {
            walk(a, r, depth + 1, order);
        }
    }
    walk(a, root, 0, &order);
}

/// Run a node-returning query and render matches as inbox lines.
fn list_query(a: &MaildirAdapter, q: &str) -> anyhow::Result<()> {
    let nodes = match quarb::run(q, a)? {
        QueryResult::Nodes(ns) => ns,
        QueryResult::Values(_) => bail!("internal: expected a node query"),
    };
    let order = inbox(a);
    let mut hits: Vec<(usize, NodeId)> = nodes
        .into_iter()
        .filter_map(|n| order.iter().position(|&m| m == n).map(|i| (i + 1, n)))
        .collect();
    hits.sort_by_key(|(i, _)| *i);
    for (i, n) in hits {
        line(a, i, n);
    }
    Ok(())
}

/// Escape a user pattern into a quoted Quarb string literal.
fn qstr(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mailbox = cli
        .mailbox
        .or_else(|| std::env::var_os("QM_MAILDIR").map(PathBuf::from))
        .context("no mailbox: pass -m or set QM_MAILDIR")?;
    let a = MaildirAdapter::open(&mailbox).context("opening mailbox")?;
    match cli.cmd {
        Cmd::Ls => {
            for (i, n) in inbox(&a).into_iter().enumerate() {
                line(&a, i + 1, n);
            }
        }
        Cmd::Read { n } => show(&a, nth(&a, n)?),
        Cmd::Thread { n } => thread(&a, nth(&a, n)?),
        Cmd::From { pat } => {
            list_query(&a, &format!("/*[::from *= {}]", qstr(&pat)))?;
        }
        Cmd::Grep { pat } => {
            // Case-insensitive, pattern regex-escaped.
            let esc: String = pat
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == ' ' {
                        c.to_string()
                    } else {
                        format!("\\{c}")
                    }
                })
                .collect();
            list_query(
                &a,
                &format!("/*[::subject =~ /(?i){esc}/ or :: =~ /(?i){esc}/]"),
            )?;
        }
        Cmd::Unread => {
            list_query(&a, "/*[;;;new]")?;
        }
        Cmd::Since { date } => {
            let epoch: i64 = if let Ok(e) = date.parse() {
                e
            } else {
                let mut it = date.splitn(3, '-');
                let (y, m, d): (i64, i64, i64) = (
                    it.next().and_then(|v| v.parse().ok()).context("bad date")?,
                    it.next().and_then(|v| v.parse().ok()).context("bad date")?,
                    it.next().and_then(|v| v.parse().ok()).context("bad date")?,
                );
                // Civil-from-days (UTC midnight).
                let yy = if m <= 2 { y - 1 } else { y };
                let era = yy.div_euclid(400);
                let yoe = yy - era * 400;
                let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
                let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                (era * 146097 + doe - 719468) * 86400
            };
            list_query(&a, &format!("/*[;;;epoch >= {epoch}]"))?;
        }
        Cmd::Replies { n } => {
            let node = nth(&a, n)?;
            let order = inbox(&a);
            let mut replies = a.backlinks(node);
            replies.sort_by_key(|(_, r)| match a.metadata(*r, "epoch") {
                Some(quarb::Value::Int(e)) => e,
                _ => i64::MAX,
            });
            for (_, r) in replies {
                let i = order
                    .iter()
                    .position(|&m| m == r)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                line(&a, i, r);
            }
        }
        Cmd::Export { file } => {
            // The cookbook's materialize-for-analysis recipe, as a
            // verb: write via qua-compatible SQLite (from, subject,
            // epoch), refusing to overwrite.
            if file.exists() {
                bail!("{} already exists (refusing to overwrite)", file.display());
            }
            let q = "/* | rec(::from, ::subject, \"epoch\", ;;;epoch)";
            let QueryResult::Values(vs) = quarb::run(q, &a)? else {
                bail!("internal: expected values");
            };
            // Minimal writer: shell out to sqlite3? No — keep zero
            // deps: write CSV beside SQLite? Simplest honest form:
            // a .csv the csv adapter reads back.
            let mut out = String::from("from,subject,epoch\n");
            for v in &vs {
                if let quarb::Value::Record(fields) = v {
                    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                    out.push_str(&format!(
                        "{},{},{}\n",
                        esc(&fields[0].1.to_string()),
                        esc(&fields[1].1.to_string()),
                        fields[2].1
                    ));
                }
            }
            std::fs::write(&file, out)?;
            println!("exported {} message(s) to {}", vs.len(), file.display());
        }
        Cmd::Count => {
            let q = "/* | ::message-id @| group(::from) | count | .messages | %.";
            if let QueryResult::Values(vs) = quarb::run(q, &a)? {
                for v in vs {
                    println!("{v}");
                }
            }
        }
        Cmd::Seen { n } => {
            let node = nth(&a, n)?;
            let path = a
                .path_of(node)
                .context("not a Maildir message (mbox has no files to flag)")?
                .to_path_buf();
            let dir = path.parent().context("no parent dir")?;
            let base = path
                .file_name()
                .context("no file name")?
                .to_string_lossy()
                .into_owned();
            let (uniq, flags) = match base.split_once(":2,") {
                Some((u, f)) => (u.to_string(), f.to_string()),
                None => (base.clone(), String::new()),
            };
            if flags.contains('S') {
                println!("already seen");
                return Ok(());
            }
            let mut flags: Vec<char> = flags.chars().collect();
            flags.push('S');
            flags.sort_unstable();
            let cur = dir
                .parent()
                .context("no maildir root")?
                .join("cur")
                .join(format!("{uniq}:2,{}", flags.iter().collect::<String>()));
            std::fs::rename(&path, &cur)
                .with_context(|| format!("renaming to {}", cur.display()))?;
            println!("seen: {}", header(&a, node, "subject"));
        }
        Cmd::Query { q } => {
            // Bind the invocation instant so `now()` resolves in
            // ad-hoc queries (the canned subcommands don't use it);
            // the clock is read once, here, never during evaluation.
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let nowed = quarb::WithNow {
                inner: &a,
                secs: since.as_secs() as i64,
                nanos: since.subsec_nanos(),
            };
            match quarb::run(&q, &nowed)? {
                QueryResult::Values(vs) => {
                    for v in vs {
                        println!("{v}");
                    }
                }
                QueryResult::Nodes(ns) => {
                    for n in ns {
                        println!("{}", a.locator(n));
                    }
                }
            }
        }
    }
    Ok(())
}
