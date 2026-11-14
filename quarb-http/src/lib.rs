//! The HTTP client the adapters share. Every request goes through
//! one agent per thread with a connection timeout and a read/write
//! timeout, so a service that stops answering fails the query with
//! a message instead of hanging it. `QUARB_HTTP_TIMEOUT` (seconds)
//! sets the read/write timeout; the connection timeout is 30 s.
//!
//! The offline switch lives here too, since this is the door to the
//! network: under `QUARB_OFFLINE=1` the agent resolves no name, and
//! under `QUARB_OFFLINE=localhost` only this machine's own
//! addresses; `QUARB_OFFLINE=0` is the switch off, as unset. A front-end refuses a target at the mount, in plain
//! words, with [`offline`], [`check_url`] and [`check_tool`]; the
//! resolver is what stands behind that for every request an adapter
//! makes later (a page of results, a link followed).

pub use ureq;

/// The variable of the offline switch.
pub const OFFLINE_VAR: &str = "QUARB_OFFLINE";

/// What `QUARB_OFFLINE` asks for: a gate's promise that a run reads
/// recorded data alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offline {
    /// Unset, or `QUARB_OFFLINE=0`: the network is reached as ever.
    Off,
    /// `QUARB_OFFLINE=1`: nothing on the network is reached, this
    /// machine's own addresses included.
    All,
    /// `QUARB_OFFLINE=localhost`: nothing but this machine's own
    /// addresses (a recorded server a gate started).
    Localhost,
}

/// The switch as the environment sets it: `1` on, `0` off (ruling
/// 2026-11-14: if 1 is true, 0 is false), `localhost` the middle
/// setting. Any other value, the empty one too, is an error, in the
/// words to print: a switch that fails closed does not guess at
/// `true` or `yes`.
pub fn offline() -> Result<Offline, String> {
    match std::env::var_os(OFFLINE_VAR) {
        None => Ok(Offline::Off),
        Some(v) => offline_from(&v.to_string_lossy()),
    }
}

/// [`offline`] for a value as written.
pub fn offline_from(value: &str) -> Result<Offline, String> {
    match value {
        "0" => Ok(Offline::Off),
        "1" => Ok(Offline::All),
        "localhost" => Ok(Offline::Localhost),
        other => Err(format!(
            "{OFFLINE_VAR}={other:?}: the switch takes 1 (nothing on the network is reached), \
             localhost (nothing but this machine's own addresses) or 0 (off: the network is \
             reached, as with the variable unset)"
        )),
    }
}

/// Whether a host, as a target writes it, is this machine: the name
/// `localhost`, or an address in 127.0.0.0/8 or `::1`. A name is
/// not resolved: another name for this machine is the network.
pub fn this_machine(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.');
    let h = h
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h);
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => a.is_loopback(),
        Ok(std::net::IpAddr::V6(a)) => {
            a.is_loopback() || a.to_ipv4_mapped().is_some_and(|a| a.is_loopback())
        }
        Err(_) => false,
    }
}

/// The hosts a URL's authority names, userinfo and ports taken off:
/// one for most, several where a target lists them with commas
/// (Kafka's brokers, a replica set). A text with no `://` is read
/// as an authority already.
pub fn url_hosts(url: &str) -> Vec<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let hosts = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    hosts.split(',').map(|h| host_of(h).to_string()).collect()
}

/// `host` of `host:port`, the brackets of an IPv6 literal kept off.
fn host_of(netloc: &str) -> &str {
    let n = netloc.trim();
    if let Some(rest) = n.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(h, _)| h);
    }
    match n.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => n,
    }
}

/// Why `hosts` may not be reached under `mode`, or `None` when they
/// may. An empty list or an empty host is refused under the switch:
/// what a driver falls back to is not written in the target.
pub fn refusal(mode: Offline, hosts: &[String]) -> Option<String> {
    match mode {
        Offline::Off => None,
        Offline::All => Some(format!(
            "{OFFLINE_VAR}=1 is set, and nothing on the network is reached under it \
             ({OFFLINE_VAR}=localhost lets this machine's own addresses through)"
        )),
        Offline::Localhost => {
            let outside = hosts.iter().find(|h| !this_machine(h));
            match outside {
                None if !hosts.is_empty() => None,
                Some(h) if !h.is_empty() => Some(format!(
                    "{OFFLINE_VAR}=localhost is set, and {h} is not this machine \
                     (localhost, 127.0.0.0/8 and ::1 are)"
                )),
                _ => Some(format!(
                    "{OFFLINE_VAR}=localhost is set, and the target names no host: \
                     write localhost for this machine"
                )),
            }
        }
    }
}

/// Whether a URL may be fetched under the switch: `Err` holds the
/// refusal, the URL named.
pub fn check_url(url: &str) -> Result<(), String> {
    match refusal(offline()?, &url_hosts(url)) {
        None => Ok(()),
        Some(why) => Err(format!("{url}: {why}")),
    }
}

/// Whether a program that reaches a cloud (`gh`, `kubectl`, `az`,
/// `gcloud`) may be run under the switch: only when its stand-in
/// variable names a recorded one to run in its place. `instead`
/// says what else lets the caller through (a token variable), for
/// the message.
pub fn check_tool(tool: &str, stand_in: Option<&str>, instead: Option<&str>) -> Result<(), String> {
    let mode = offline()?;
    if mode == Offline::Off || stand_in.is_some_and(|v| std::env::var_os(v).is_some()) {
        return Ok(());
    }
    let value = if mode == Offline::All {
        "1"
    } else {
        "localhost"
    };
    let mut way = Vec::new();
    if let Some(v) = stand_in {
        way.push(format!("{v} names a recorded stand-in to run in its place"));
    }
    if let Some(i) = instead {
        way.push(i.to_string());
    }
    Err(format!(
        "{OFFLINE_VAR}={value} is set, and {tool} reaches the network, so it is not run{}",
        if way.is_empty() {
            String::new()
        } else {
            format!(" ({})", way.join("; "))
        }
    ))
}

/// The resolver behind every request: the standard one, under the
/// switch. A refusal is an I/O error whose text names the variable.
struct Guarded;

impl ureq::Resolver for Guarded {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        resolve_under(offline().map_err(std::io::Error::other)?, netloc)
    }
}

/// `netloc` (`host:port`) resolved under `mode`: refused by what is
/// written before any name is looked up, and under `localhost` held
/// to this machine's addresses after.
fn resolve_under(mode: Offline, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    if let Some(why) = refusal(mode, &[host_of(netloc).to_string()]) {
        return Err(std::io::Error::other(why));
    }
    let addrs: Vec<_> = netloc.to_socket_addrs()?.collect();
    if mode != Offline::Localhost {
        return Ok(addrs);
    }
    // the name was this machine's; so must its addresses be
    let own: Vec<_> = addrs
        .into_iter()
        .filter(|a| this_machine(&a.ip().to_string()))
        .collect();
    if own.is_empty() {
        return Err(std::io::Error::other(format!(
            "{OFFLINE_VAR}=localhost is set, and {netloc} resolves to no address of this machine"
        )));
    }
    Ok(own)
}

/// The read/write timeout, in seconds, unless `QUARB_HTTP_TIMEOUT`
/// says otherwise.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

thread_local! {
    static AGENT: ureq::Agent = build();
}

fn build() -> ureq::Agent {
    let secs = std::env::var("QUARB_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(secs))
        .timeout_write(std::time::Duration::from_secs(secs))
        .resolver(Guarded)
        .build()
}

/// The shared agent: `quarb_http::agent().get(url).call()`.
pub fn agent() -> ureq::Agent {
    AGENT.with(|a| a.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_takes_three_values() {
        assert_eq!(offline_from("1"), Ok(Offline::All));
        assert_eq!(offline_from("localhost"), Ok(Offline::Localhost));
        // if 1 is true, 0 is false: the switch off, as unset
        assert_eq!(offline_from("0"), Ok(Offline::Off));
        for other in [
            "",
            "true",
            "false",
            "yes",
            "no",
            "loopback",
            "LOCALHOST",
            " 1",
            "00",
        ] {
            let e = offline_from(other).unwrap_err();
            assert!(
                e.contains("takes 1") && e.contains("localhost") && e.contains("or 0"),
                "{e}"
            );
        }
    }

    #[test]
    fn this_machine_is_told_by_what_is_written() {
        for h in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "127.0.0.1",
            "127.8.9.10",
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
        ] {
            assert!(this_machine(h), "{h}");
        }
        for h in [
            "",
            "example.org",
            "localhost.example.org",
            "10.0.0.1",
            "0.0.0.0",
            "192.168.1.1",
            "::",
            "128.0.0.1",
            "ithaca",
        ] {
            assert!(!this_machine(h), "{h}");
        }
    }

    #[test]
    fn a_url_names_its_hosts() {
        let hosts = |u: &str| url_hosts(u);
        assert_eq!(hosts("http://127.0.0.1:8080/logs?x=1"), ["127.0.0.1"]);
        assert_eq!(hosts("https://example.org"), ["example.org"]);
        assert_eq!(
            hosts("postgres://ada:p%40ss@db.example.org:5432/app"),
            ["db.example.org"]
        );
        assert_eq!(hosts("http://[::1]:9000/x"), ["::1"]);
        assert_eq!(hosts("kafka://b1:9092,b2:9092?topics=a"), ["b1", "b2"]);
        // LDAP's bind name holds commas and equals signs
        assert_eq!(
            hosts("ldap://cn=admin,dc=tesslab,dc=org:pw@localhost:3890/dc=tesslab,dc=org"),
            ["localhost"]
        );
        assert_eq!(hosts("localhost:5000"), ["localhost"]);
        assert_eq!(hosts("redis:///0"), [""]);
    }

    #[test]
    fn the_resolver_is_held_to_the_switch() {
        // refused by what is written: no name is looked up
        let e = resolve_under(Offline::All, "127.0.0.1:80").unwrap_err();
        assert!(e.to_string().contains("QUARB_OFFLINE=1"), "{e}");
        let e = resolve_under(Offline::Localhost, "db.example.org:5432").unwrap_err();
        assert!(
            e.to_string().contains("db.example.org is not this machine"),
            "{e}"
        );
        // this machine's own addresses resolve
        let own = resolve_under(Offline::Localhost, "127.0.0.1:8080").unwrap();
        assert_eq!(own, ["127.0.0.1:8080".parse().unwrap()]);
        assert!(
            !resolve_under(Offline::Localhost, "[::1]:8080")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            resolve_under(Offline::Off, "127.0.0.1:8080").unwrap(),
            ["127.0.0.1:8080".parse().unwrap()]
        );
    }

    #[test]
    fn a_refusal_names_the_variable() {
        let h = |s: &str| vec![s.to_string()];
        assert_eq!(refusal(Offline::Off, &h("example.org")), None);
        assert_eq!(refusal(Offline::Localhost, &h("127.0.0.1")), None);
        let e = refusal(Offline::All, &h("127.0.0.1")).unwrap();
        assert!(
            e.contains("QUARB_OFFLINE=1") && e.contains("QUARB_OFFLINE=localhost"),
            "{e}"
        );
        let e = refusal(Offline::Localhost, &h("example.org")).unwrap();
        assert!(e.contains("example.org is not this machine"), "{e}");
        // every host of a list must be this machine
        let two = vec!["localhost".to_string(), "b2.example.org".to_string()];
        assert!(
            refusal(Offline::Localhost, &two)
                .unwrap()
                .contains("b2.example.org")
        );
        // no host written: refused, the way through said
        for none in [vec![], h("")] {
            let e = refusal(Offline::Localhost, &none).unwrap();
            assert!(e.contains("names no host"), "{e}");
        }
    }
}
