//! The offline switch at the command line (`QUARB_OFFLINE`, approved
//! 2026-11-13; the second value ruled the same day): under `1`
//! nothing on the network is reached, under `localhost` nothing but
//! this machine's own addresses, `0` is the switch off, any other
//! value refuses every mount, and a cloud's own program runs only
//! as its recorded stand-in.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `qua ARGS` under `QUARB_OFFLINE=offline` (unset for `None`) and
/// the further variables given.
fn run(offline: Option<&str>, env: &[(&str, &str)], args: &[&str]) -> (String, String, bool) {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_qua"));
    c.env("QUARB_LOCALE", "C").env_remove("QUARB_OFFLINE");
    for v in ["QUARB_GH", "QUARB_GLAB", "QUARB_KUBECTL", "QUARB_GCLOUD"] {
        c.env_remove(v);
    }
    if let Some(v) = offline {
        c.env("QUARB_OFFLINE", v);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.success(),
    )
}

/// The refusal a run ends with.
fn refused(offline: &str, env: &[(&str, &str)], args: &[&str]) -> String {
    let (out, err, ok) = run(Some(offline), env, args);
    assert!(!ok, "qua {args:?} under {offline}: answered {out}");
    assert!(err.contains("QUARB_OFFLINE"), "qua {args:?}: {err}");
    err
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-offline-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_file_on_the_disk_reads_as_ever() {
    let dir = scratch("disk");
    let f = dir.join("notes.json");
    std::fs::write(&f, r#"{"keeper": "Callimachus"}"#).unwrap();
    let f = f.to_str().unwrap();
    for mode in [None, Some("0"), Some("1"), Some("localhost")] {
        let (out, err, ok) = run(mode, &[], &["/keeper::", f]);
        assert!(ok, "{mode:?}: {err}");
        assert_eq!(out.trim(), "Callimachus", "{mode:?}");
    }
    // a query with no source asks for nothing
    let (out, _, ok) = run(Some("1"), &[], &["1 + 1"]);
    assert!(ok && out.trim() == "2", "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn any_other_value_refuses_every_mount() {
    let dir = scratch("value");
    let f = dir.join("notes.json");
    std::fs::write(&f, r#"{"keeper": "Callimachus"}"#).unwrap();
    for value in ["yes", "true", "false", "", "loopback", "LOCALHOST"] {
        let err = refused(value, &[], &["/keeper::", f.to_str().unwrap()]);
        assert!(
            err.contains("the switch takes 1") && err.contains("localhost") && err.contains("or 0"),
            "{value:?}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn under_one_nothing_on_the_network_is_reached() {
    // this machine's own addresses included, on every door: the
    // opener's mount, qua's own relational doors, the web level, an
    // aliased mount among several
    for target in [
        "postgres://ada:s3cret@localhost:5432/app",
        "host=127.0.0.1 dbname=app",
        "mysql://root@127.0.0.1:3306/shop",
        "redis://localhost:6379",
        "kafka://localhost:9092",
        "imap://127.0.0.1:1143",
        "sparql:http://localhost:3030/query",
        "s3://lake?endpoint=http://localhost:9000",
        "cwl:?since=2d&endpoint=http://127.0.0.1:4588/",
        "web:postgres://postgres@127.0.0.1:5432/site",
        "db=postgres://localhost/app",
    ] {
        let err = refused("1", &[], &["/*", target]);
        assert!(err.contains("QUARB_OFFLINE=1 is set"), "{target}: {err}");
        assert!(err.contains("QUARB_OFFLINE=localhost"), "{target}: {err}");
        // a password is never printed
        assert!(!err.contains("s3cret"), "{target}: {err}");
    }
    // priced before it is opened: refused all the same
    let err = refused("1", &[], &["--plan", "/*", "postgres://localhost/app"]);
    assert!(err.contains("QUARB_OFFLINE=1 is set"), "{err}");
}

#[test]
fn under_localhost_the_network_is_refused_by_name() {
    for (target, says) in [
        (
            "postgres://ada:s3cret@db.example.org/app",
            "db.example.org is not this machine",
        ),
        (
            "host=db.example.org dbname=app",
            "db.example.org is not this machine",
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
            "sparql:https://query.example.org/sparql",
            "query.example.org",
        ),
        ("s3://lake", "a service on the network"),
        ("bigquery://project/dataset", "a service on the network"),
        ("redis:///0", "names no host"),
    ] {
        let err = refused("localhost", &[], &["/*", target]);
        assert!(err.contains(says), "{target}: {err}");
        assert!(
            err.contains("QUARB_OFFLINE=localhost is set"),
            "{target}: {err}"
        );
        assert!(!err.contains("s3cret"), "{target}: {err}");
    }
}

/// A listener on this machine that counts the connections it takes
/// and answers each with a server error: enough to see whether a
/// run reached it.
fn listener() -> (u16, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            h.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (port, hits)
}

#[test]
fn under_localhost_a_server_on_this_machine_is_reached() {
    let (port, hits) = listener();
    let target = format!("sparql:http://127.0.0.1:{port}/query");
    // under 1 the mount is refused and nothing connects
    let err = refused("1", &[], &["/*", &target]);
    assert!(err.contains("QUARB_OFFLINE=1 is set"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    // under localhost the request goes out, through the shared
    // agent's resolver, and the error is the server's
    let (_, err, ok) = run(Some("localhost"), &[], &["/*", &target]);
    assert!(!ok);
    assert!(!err.contains("QUARB_OFFLINE"), "{err}");
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "nothing reached the listener: {err}"
    );
    // and with the switch off, as ever: unset, or 0 (if 1 is true,
    // 0 is false, ruling 2026-11-14)
    for off in [None, Some("0")] {
        let before = hits.load(Ordering::SeqCst);
        let (_, err, _) = run(off, &[], &["/*", &target]);
        assert!(!err.contains("QUARB_OFFLINE"), "{off:?}: {err}");
        assert!(hits.load(Ordering::SeqCst) > before, "{off:?}: {err}");
    }
}

/// A recorded cluster of one pod: the stand-in `QUARB_KUBECTL`
/// names, as the documentation's `kubectl-fixture` is.
#[cfg(unix)]
fn stand_in(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let snap = dir.join("kubernetes-snapshot");
    std::fs::create_dir_all(&snap).unwrap();
    std::fs::write(
        snap.join("_api_v1.json"),
        r#"{"kind": "APIResourceList", "groupVersion": "v1", "resources": [
            {"name": "pods", "namespaced": true, "kind": "Pod", "verbs": ["get", "list"]}]}"#,
    )
    .unwrap();
    std::fs::write(
        snap.join("_api_v1_pods.json"),
        r#"{"kind": "PodList", "apiVersion": "v1", "items": [
            {"metadata": {"name": "loom-6d4f8b7c9-k2x7p", "namespace": "ithaca",
                          "creationTimestamp": "2026-03-18T21:40:05Z"},
             "spec": {"nodeName": "eumaeus"}, "status": {"phase": "Running"}}]}"#,
    )
    .unwrap();
    let script = dir.join("kubectl-fixture");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         [ \"$1\" = --context ] && shift 2\n\
         [ \"$1\" = get ] && [ \"$2\" = --raw ] && [ -n \"$3\" ] || exit 2\n\
         f=\"$(dirname \"$0\")/kubernetes-snapshot/$(printf %s \"${3%%\\?*}\" | tr / _).json\"\n\
         [ -f \"$f\" ] || { echo \"HTTP 404: no snapshot for $3\" >&2; exit 1; }\n\
         cat \"$f\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_string()
}

#[cfg(unix)]
#[test]
fn a_cloud_tool_runs_only_as_its_stand_in() {
    let dir = scratch("tool");
    let kubectl = stand_in(&dir);
    for mode in ["1", "localhost"] {
        // with no stand-in the tool is not run
        let err = refused(mode, &[], &["/pods/* | :name", "k8s:"]);
        assert!(
            err.contains("runs kubectl") && err.contains("QUARB_KUBECTL"),
            "{mode}: {err}"
        );
        // another tool's stand-in is no leave
        let err = refused(
            mode,
            &[("QUARB_GH", &kubectl)],
            &["/pods/* | :name", "k8s:"],
        );
        assert!(err.contains("QUARB_KUBECTL"), "{mode}: {err}");
        // its own lets the target through, and the stand-in answers
        let (out, err, ok) = run(
            Some(mode),
            &[("QUARB_KUBECTL", &kubectl)],
            &["/pods/* | %(:name; ::phase; ::node)", "k8s:ithaca"],
        );
        assert!(ok, "{mode}: {err}");
        assert_eq!(
            out.trim(),
            r#"%(name = "loom-6d4f8b7c9-k2x7p"; phase = "Running"; node = "eumaeus")"#,
            "{mode}"
        );
    }
    for (target, tool, var) in [
        ("github:rust-lang/rust", "gh", "QUARB_GH"),
        ("gitlab:gitlab-org/gitlab", "glab", "QUARB_GLAB"),
        ("gcl:my-project?since=1d", "gcloud", "QUARB_GCLOUD"),
    ] {
        let err = refused("1", &[], &["/*", target]);
        assert!(
            err.contains(&format!("runs {tool}")) && err.contains(var),
            "{target}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
