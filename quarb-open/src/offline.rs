//! The offline switch at the mount: `QUARB_OFFLINE`.
//!
//! A gate that replays recorded data sets the variable, and from
//! then on a target that would reach the network is refused where
//! it is mounted, in plain words, before anything is read:
//!
//! - `QUARB_OFFLINE=1`: nothing on the network is reached, this
//!   machine's own addresses included;
//! - `QUARB_OFFLINE=localhost`: nothing but this machine's own
//!   addresses (`localhost`, 127.0.0.0/8, `::1`), for the recorded
//!   servers a gate starts;
//! - `QUARB_OFFLINE=0`: off, as with the variable unset (if 1 is
//!   true, 0 is false);
//! - any other value, the empty one too, refuses every mount: a
//!   switch that fails closed does not guess.
//!
//! A target that runs a cloud's own program (`gh`, `glab`,
//! `kubectl`, `gcloud`) is let through when the variable that names
//! a recorded stand-in for it is set; the stand-in is then what
//! runs.
//!
//! One table says how each kind of target names what it reaches
//! ([`OUTSIDE`]); a new network adapter gets a row. What the table
//! lets through is still held by the shared HTTP agent's resolver
//! (`quarb-http`) on every request made later. Out of the switch's
//! reach, by design: a program the user names (`serve:`, a query's
//! own commands), and a local file or folder, whatever it holds.

use quarb_http::{Offline, refusal, url_hosts};

/// How a target names what it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// `scheme://[user[:pass]@]host[:port]/…`: the authority's
    /// host, or hosts (Kafka's brokers, a replica set).
    Host,
    /// A URL follows the prefix: `sparql:http://host/query`.
    Url,
    /// A cloud service at its own address, unless the option
    /// `endpoint=URL` names another (an emulator, a recorded
    /// server).
    Service,
    /// `endpoint=URL` where the target has one, else the
    /// authority's host.
    EndpointOrHost,
    /// libpq's keyword form: `host=… dbname=…`.
    Keywords,
    /// A program is run for the target; the variable names a
    /// recorded stand-in for it.
    Tool(&'static str, &'static str),
}

/// Every prefix under which a target leaves this machine. Longer
/// prefixes first where one opens another (`kafka://`, `kafka:`).
pub const OUTSIDE: &[(&str, Reach)] = &[
    // relational and document stores
    ("postgres://", Reach::Host),
    ("postgresql://", Reach::Host),
    ("host=", Reach::Keywords),
    ("mysql://", Reach::Host),
    ("mssql://", Reach::Host),
    ("oracle://", Reach::Host),
    ("mongodb://", Reach::Host),
    ("mongodb+srv://", Reach::Host),
    ("redis://", Reach::Host),
    ("rediss://", Reach::Host),
    // graph stores
    ("neo4j://", Reach::Host),
    ("neo4j+s://", Reach::Host),
    ("falkor://", Reach::Host),
    ("memgraph://", Reach::Host),
    ("arango://", Reach::Host),
    ("age://", Reach::Host),
    ("neptune://", Reach::EndpointOrHost),
    ("sparql:", Reach::Url),
    // directories, queues, mail, the web
    ("ldap://", Reach::Host),
    ("ldaps://", Reach::Host),
    ("kafka://", Reach::Host),
    ("kafka:", Reach::Host),
    ("imap://", Reach::Host),
    ("imaps://", Reach::Host),
    ("http://", Reach::Host),
    ("https://", Reach::Host),
    // cloud services
    ("gs://", Reach::Service),
    ("s3://", Reach::Service),
    ("az://", Reach::Service),
    ("dynamodb:", Reach::Service),
    ("cosmos://", Reach::Service),
    ("athena:", Reach::Service),
    ("firestore://", Reach::Service),
    ("datastore://", Reach::Service),
    ("firebase://", Reach::Service),
    ("bigquery://", Reach::Service),
    ("gsheet://", Reach::Service),
    // cloud logs
    ("cwl:", Reach::Service),
    ("cloudwatch:", Reach::Service),
    ("ddl:", Reach::Service),
    ("datadog:", Reach::Service),
    ("azl:", Reach::Service),
    ("azlogs:", Reach::Service),
    ("cfl:", Reach::Service),
    ("cflogs:", Reach::Service),
    ("gcl:", Reach::Tool("gcloud", "QUARB_GCLOUD")),
    ("gcplogs:", Reach::Tool("gcloud", "QUARB_GCLOUD")),
    // forges and clusters, through their own programs
    ("github:", Reach::Tool("gh", "QUARB_GH")),
    ("gitlab:", Reach::Tool("glab", "QUARB_GLAB")),
    ("k8s:", Reach::Tool("kubectl", "QUARB_KUBECTL")),
    ("kubernetes:", Reach::Tool("kubectl", "QUARB_KUBECTL")),
];

/// The reading prefixes a target may wear over what it reaches
/// (`web:postgres://…`): the switch looks under them.
const READINGS: &[&str] = &[
    "web:", "text:", "corpus:", "lit:", "koine:", "lines:", "code:",
];

/// How `target` reaches outside, or `None` for a target on this
/// machine's disk.
pub fn reach(target: &str) -> Option<(&'static str, Reach)> {
    let mut t = target;
    while let Some(rest) = READINGS.iter().find_map(|r| t.strip_prefix(r)) {
        t = rest;
    }
    OUTSIDE
        .iter()
        .find(|(prefix, _)| t.starts_with(prefix))
        .map(|&(prefix, r)| (prefix, r))
}

/// The switch at a mount: `Ok` when `target` may be opened. Every
/// mount passes here; a target on the disk costs one look at the
/// environment.
pub fn gate(target: &str) -> anyhow::Result<()> {
    let mode = quarb_http::offline().map_err(|e| anyhow::anyhow!("{e}"))?;
    match refused(target, mode, |v| std::env::var_os(v).is_some()) {
        None => Ok(()),
        Some(why) => anyhow::bail!("{}: {why}", quarb::target::redact(target)),
    }
}

/// Why `target` is refused under `mode`, or `None`. `set` answers
/// whether a stand-in variable is set.
pub fn refused(target: &str, mode: Offline, set: impl Fn(&str) -> bool) -> Option<String> {
    if mode == Offline::Off {
        return None;
    }
    let (prefix, reach) = reach(target)?;
    let under = &target[target.find(prefix)?..];
    let value = if mode == Offline::All {
        "1"
    } else {
        "localhost"
    };
    let var = quarb_http::OFFLINE_VAR;
    match reach {
        Reach::Tool(tool, stand_in) => (!set(stand_in)).then(|| {
            format!(
                "{var}={value} is set, and this target runs {tool}, which reaches the network \
                 ({stand_in} names a recorded stand-in to run in its place)"
            )
        }),
        Reach::Host => refusal(mode, &url_hosts(after(under, prefix))),
        Reach::Url => refusal(mode, &url_hosts(&under[prefix.len()..])),
        Reach::Keywords => refusal(mode, &keyword_hosts(under)),
        Reach::Service => match endpoint(under) {
            Some(e) => refusal(mode, &url_hosts(e)),
            None if mode == Offline::All => refusal(mode, &[]),
            None => Some(format!(
                "{var}=localhost is set, and this target is a service on the network \
                 (an endpoint= on this machine is let through)"
            )),
        },
        Reach::EndpointOrHost => match endpoint(under) {
            Some(e) => refusal(mode, &url_hosts(e)),
            None => refusal(mode, &url_hosts(after(under, prefix))),
        },
    }
}

/// What follows the prefix, as [`url_hosts`] reads an authority:
/// `kafka:b1:9092` has no `//` to skip.
fn after<'a>(target: &'a str, prefix: &str) -> &'a str {
    let rest = &target[prefix.len()..];
    rest.strip_prefix("//").unwrap_or(rest)
}

/// The value of a target's `endpoint=` option.
fn endpoint(target: &str) -> Option<&str> {
    let (_, query) = target.split_once('?')?;
    // the option's value is a URL, and may carry a `#` of its own
    // only in a fragment after it
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("endpoint="))
        .map(|v| v.split('#').next().unwrap_or(v))
}

/// The hosts of libpq's keyword form: `host=` and `hostaddr=`, each
/// a comma list. A host that opens with `/` is a socket's folder,
/// on this machine.
fn keyword_hosts(target: &str) -> Vec<String> {
    target
        .split_whitespace()
        .filter_map(|kv| {
            kv.strip_prefix("host=")
                .or_else(|| kv.strip_prefix("hostaddr="))
        })
        .flat_map(|v| v.split(','))
        .map(|h| {
            let h = h.trim_matches('\'');
            if h.starts_with('/') {
                "localhost".to_string()
            } else {
                h.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn a_target_on_the_disk_is_never_refused() {
        for t in [
            "notes.json",
            "/srv/data",
            "text:hymn.md",
            "library.db",
            "git:.",
            "mail:/var/mail/ada",
            "serve:./server",
            "kuzu:graph.kz",
            "web:site.db",
            "C:\\data\\x.csv",
        ] {
            assert_eq!(reach(t), None, "{t}");
            assert_eq!(refused(t, Offline::All, none), None, "{t}");
        }
    }

    #[test]
    fn off_refuses_nothing() {
        for (prefix, _) in OUTSIDE {
            let t = format!("{prefix}db.example.org/x");
            assert_eq!(refused(&t, Offline::Off, none), None, "{t}");
        }
    }

    #[test]
    fn under_one_every_row_is_refused() {
        // this machine's own addresses too, and an endpoint on them
        for (prefix, r) in OUTSIDE {
            let t = match r {
                Reach::Keywords => "host=localhost dbname=app".to_string(),
                Reach::Url => format!("{prefix}http://localhost:3030/q"),
                Reach::Service | Reach::EndpointOrHost => {
                    format!("{prefix}x?endpoint=http://127.0.0.1:4566/")
                }
                _ => format!("{prefix}localhost/x"),
            };
            let why = refused(&t, Offline::All, none).unwrap_or_else(|| panic!("{t} let through"));
            assert!(why.contains("QUARB_OFFLINE=1"), "{t}: {why}");
        }
    }

    #[test]
    fn under_localhost_this_machine_is_let_through() {
        for t in [
            "postgres://ada:pw@localhost:5432/app",
            "postgresql://127.0.0.1/app",
            "host=localhost port=5433 dbname=app",
            "host=/var/run/postgresql dbname=app",
            "mysql://root@127.0.0.1:3306/shop",
            "mongodb://localhost:27017/music",
            "redis://localhost:6379",
            "neo4j://localhost/neo4j?key=id",
            "memgraph://localhost:7687?key=name",
            "falkor://localhost:6379/office?key=name",
            "arango://root:pw@localhost:8529/shop",
            "age://postgres:pw@localhost:5455/postgres",
            "sparql:http://localhost:3030/query#key=rdfs:label",
            "ldap://cn=admin,dc=tesslab,dc=org:pw@localhost:3890/dc=tesslab,dc=org",
            "kafka://localhost:9092",
            "kafka:localhost:9092?topics=events,users",
            "kafka://quarb:secret@localhost:9093?sasl=scram-sha-256",
            "imap://127.0.0.1:1143",
            "http://127.0.0.1:8080/feed.json",
            "https://[::1]:8443/",
            "s3://lake?endpoint=http://localhost:9000",
            "dynamodb://?endpoint=http://localhost:8000",
            "cosmos://shop/shop?endpoint=http://127.0.0.1:8081",
            "athena://music?endpoint=http://127.0.0.1:4566",
            "cwl:?since=2d&endpoint=http://127.0.0.1:4588/",
            "ddl:?since=1d&query=service:checkout&endpoint=http://127.0.0.1:4599/",
            "azl:ws-demo?table=AppRequests,AppTraces&since=1d&endpoint=http://127.0.0.1:4600/",
            "cfl:zone9?since=1d&endpoint=http://127.0.0.1:4601/",
            "neptune://db.example.org:8182?endpoint=http://127.0.0.1:8182",
            "neptune://localhost:8182",
            "web:postgres://postgres@127.0.0.1:5432/site",
        ] {
            assert_eq!(refused(t, Offline::Localhost, none), None, "{t}");
        }
    }

    #[test]
    fn under_localhost_the_network_is_refused() {
        for (t, says) in [
            (
                "postgres://ada:pw@db.example.org/app",
                "db.example.org is not this machine",
            ),
            (
                "host=db.example.org dbname=app",
                "db.example.org is not this machine",
            ),
            (
                "host=localhost,db2.example.org dbname=app",
                "db2.example.org",
            ),
            (
                "mongodb+srv://cluster0.example.net/music",
                "cluster0.example.net",
            ),
            (
                "kafka://localhost:9092,b2.example.org:9092",
                "b2.example.org",
            ),
            (
                "sparql:https://query.wikidata.org/sparql",
                "query.wikidata.org",
            ),
            (
                "https://example.org/feed.json",
                "example.org is not this machine",
            ),
            ("web:postgres://db.example.org/site", "db.example.org"),
            // a name that is not `localhost` is not resolved
            ("redis://ithaca:6379", "ithaca is not this machine"),
            // a service with no endpoint is the cloud
            ("s3://lake", "a service on the network"),
            ("gs://bucket/prefix", "a service on the network"),
            ("bigquery://project/dataset", "a service on the network"),
            ("cwl:?since=2d", "a service on the network"),
            (
                "s3://lake?endpoint=https://minio.example.org",
                "minio.example.org",
            ),
            ("neptune://db.example.org:8182", "db.example.org"),
            // no host written: what the driver falls back to is not
            // in the target
            ("redis:///0", "names no host"),
            ("postgres:///app", "names no host"),
        ] {
            let why =
                refused(t, Offline::Localhost, none).unwrap_or_else(|| panic!("{t} let through"));
            assert!(why.contains(says), "{t}: {why}");
            assert!(why.contains("QUARB_OFFLINE=localhost"), "{t}: {why}");
        }
    }

    #[test]
    fn a_tool_runs_only_as_its_stand_in() {
        for (t, tool, var) in [
            ("github:rust-lang/rust", "gh", "QUARB_GH"),
            ("gitlab:gitlab-org/gitlab", "glab", "QUARB_GLAB"),
            ("k8s:", "kubectl", "QUARB_KUBECTL"),
            ("kubernetes:ithaca", "kubectl", "QUARB_KUBECTL"),
            ("gcl:my-project?since=1d", "gcloud", "QUARB_GCLOUD"),
        ] {
            for mode in [Offline::All, Offline::Localhost] {
                let why = refused(t, mode, none).unwrap_or_else(|| panic!("{t} let through"));
                assert!(why.contains(tool) && why.contains(var), "{t}: {why}");
                assert_eq!(refused(t, mode, |v| v == var), None, "{t}");
                // another tool's stand-in is no leave
                assert!(refused(t, mode, |v| v == "QUARB_OTHER").is_some(), "{t}");
            }
        }
    }

    #[test]
    fn a_password_is_not_printed() {
        // the refusal is built from the host alone; the target is
        // quoted redacted by `gate`
        let why = refused(
            "postgres://ada:s3cret@db.example.org/app",
            Offline::Localhost,
            none,
        )
        .unwrap();
        assert!(!why.contains("s3cret"), "{why}");
        assert_eq!(
            quarb::target::redact("postgres://ada:s3cret@db.example.org/app"),
            "postgres://ada:***@db.example.org/app"
        );
    }
}
