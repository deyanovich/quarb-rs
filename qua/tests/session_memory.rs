//! The session memory (spec: Capsa Operations, "Session memory",
//! draft 166): `| ,name` files the topic for the rest of the session,
//! `$,name` reads it wherever a register stands, `%,` is the whole
//! memory as a record; the push gathers; an unknown name refuses
//! saying which memory it looked in.
use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"rows":[{"n":1},{"n":2},{"n":3}]}"#).unwrap()
}

/// A session: one memory shared by every query run in it.
struct Session {
    memory: quarb::SessionMemory,
}

impl Session {
    fn new() -> Session {
        Session { memory: quarb::session_memory() }
    }

    fn run(&self, q: &str) -> quarb::Result<Vec<String>> {
        let run = quarb::Run {
            session: Some(self.memory.clone()),
            ..quarb::Run::ambient()
        };
        let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
        Ok(match out.result {
            QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
            QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
        })
    }

    fn one(&self, q: &str) -> String {
        let v = self.run(q).unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(v.len(), 1, "{q}: {v:?}");
        v.into_iter().next().unwrap()
    }

    fn refuse(&self, q: &str) -> String {
        self.run(q).unwrap_err().to_string()
    }
}

#[test]
fn a_session_push_files_a_value_the_session_reads_back() {
    let s = Session::new();
    // one thread files its topic; the push leaves the cursor untouched
    assert_eq!(s.one("= 41 | ,a | $_ + 1"), "42");
    assert_eq!(s.one("= $,a"), "41");
    assert_eq!(s.one("= $,a + 1"), "42");
    // several threads file the list of their topics, as `@| @`, and
    // pass on untouched, each reading the list back
    assert_eq!(s.run("/rows/* | ::n | ,ns | $,ns").unwrap(), vec!["@(1; 2; 3)"; 3]);
    assert_eq!(s.one("= $,ns | sum"), "6");
    // the whole memory as a record, in the order filed; a reassignment
    // replaces
    assert_eq!(s.one("= %,"), "%(a = 41; ns = @(1; 2; 3))");
    assert_eq!(s.one("= 7 | ,a | %, | :a"), "7");
    // a quarq filed is recalled with the splice; the rounded read
    assert_eq!(s.one("= q(* 101.2) | ,g | @(1; 2) | map(&$,g) | [2]"), "202.4");
    assert_eq!(s.one("= (,a) + 1"), "8");
    // a table filed keeps its index, and the register memory is apart
    assert_eq!(
        s.one("= #(@(date; A); %(2013-01-01; 1); %(2013-01-02; 2)) | #(-i=:date) | ,df | $,df:A"),
        "%(2013-01-01 = 1; 2013-01-02 = 2)"
    );
    assert_eq!(s.one("= $,df[[2013-01-02]] | :A"), "2");
    assert_eq!(s.one("= 5 | .a | $.a + $,a"), "12");
}

#[test]
fn an_unknown_session_name_refuses_naming_the_memory() {
    let s = Session::new();
    let e = s.refuse("= $,nope");
    assert!(e.contains("no value ',nope' in the session memory") && e.contains("'$.nope'"), "{e}");
    // a run with no session memory refuses naming the hosts
    let e = quarb::run("= $,a", &doc()).unwrap_err().to_string();
    assert!(e.contains("the session memory is not open in this run"), "{e}");
    let e = quarb::run("= 1 | ,a", &doc()).unwrap_err().to_string();
    assert!(e.contains("the session memory is not open in this run"), "{e}");
    // the canonical spelling
    assert_eq!(s.one("= q(| ,a | $,a | %, | (,a))"), "q(,a | $,a | %, | $,a)");
}

/// The session settings (spec: Session memory, "Session settings",
/// draft 166): `session` alone answers the settings record; a setting
/// answers nothing (no thread); the settings outlive the query.
#[test]
fn the_session_settings_are_set_by_the_session_command() {
    let s = Session::new();
    assert_eq!(s.run("session").unwrap(), vec![r#"%(pretty = "off"; precision = 6)"#]);
    // a trailing set yields no thread, so no line
    assert_eq!(s.run("session(--pretty=always)").unwrap(), Vec::<String>::new());
    assert_eq!(s.run("session").unwrap(), vec![r#"%(pretty = "always"; precision = 6)"#]);
    // the command heads a query, the thread on the root as after `=`
    assert_eq!(s.run("session(--precision=2) | session").unwrap(), vec![r#"%(pretty = "always"; precision = 2)"#]);
    // the session's digits are pretty's default; `-p` on the call wins
    assert_eq!(s.run("= %(a = 1.123456789) | pretty").unwrap(), vec!["%(a = 1.12)"]);
    assert_eq!(s.run("= %(a = 1.123456789) | pretty(-p=4)").unwrap(), vec!["%(a = 1.1235)"]);
    assert_eq!(s.run("session(--pretty=off) | session").unwrap(), vec![r#"%(pretty = "off"; precision = 2)"#]);
    // a wrong word and an unknown setting refuse naming the two
    let e = s.run("session(--pretty=sometimes)").unwrap_err().to_string();
    assert!(e.contains("'always', 'structures' or 'off'"), "{e}");
    let e = s.run("session(--width=3)").unwrap_err().to_string();
    assert!(e.contains("is no setting"), "{e}");
    // without a session memory the command refuses
    let e = quarb::run("session", &doc()).unwrap_err().to_string();
    assert!(e.contains("not open in this run"), "{e}");
}
