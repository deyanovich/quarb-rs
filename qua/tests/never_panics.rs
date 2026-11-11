//! One short query each once killed the process (the code review of
//! 2026-11-10): a panic, an abort on allocation, a stack overflow.
//! Each is now a refusal, or an answer, and the process exits on its
//! own terms. A crash shows here as a missing exit code (a signal)
//! or as exit 101 (a panic).

fn qua(query: &str) -> (Option<i32>, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .arg(query)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Refused in the engine's own words: exit 1, the text in stderr.
fn refused(query: &str, needle: &str) {
    let (code, out, err) = qua(query);
    assert_eq!(
        code,
        Some(1),
        "{query}: exit {code:?}, stdout {out}, stderr {err}"
    );
    assert!(err.contains(needle), "{query}: {err}");
}

fn answers(query: &str, want: &str) {
    let (code, out, err) = qua(query);
    assert_eq!(code, Some(0), "{query}: exit {code:?}, stderr {err}");
    assert_eq!(out, want, "{query}");
}

#[test]
fn a_malformed_node_id_anchor() {
    refused("= ((!1)", "to close the node-id anchor");
    for q in ["= ((!", "/a[((!))]", "= 1 + ((!"] {
        let (code, _, err) = qua(q);
        assert_eq!(code, Some(1), "{q}: {err}");
    }
}

/// A width from the query or from data sized an allocation with no
/// bound.
#[test]
fn a_padding_past_the_bound() {
    refused("= \"a\" | lpad(30000000000)", "would exceed 67108864 bytes");
    refused("= \"a\" | rpad(30000000000)", "would exceed 67108864 bytes");
    refused(
        "= \"a\" | pad(30000000000; \"é\")",
        "would exceed 67108864 bytes",
    );
    answers("= \"ab\" | lpad(5; \"*\")", "***ab");
    answers("= \"ab\" | pad(6; \"-\")", "--ab--");
}

#[test]
fn a_bin_count_and_a_bar_width_past_the_bound() {
    refused("= @(1; 2; 3) | bins(99999999999)", "at most 1048576 bins");
    refused(
        "= @(1; 2; 3) | histogram(--width=2305843009213693952)",
        "a number from 1 to 4096",
    );
}

/// The walk recursed as deep as the gram is long; the grams of a
/// long stream can also outgrow memory, and are refused past a bound.
#[test]
fn skipgrams_of_a_long_stream() {
    answers(
        "= (1..200000) | ... @| skipgrams(200000; 0) | count",
        "200000",
    );
    refused(
        "= (1..20000) | ... @| skipgrams(5000; 0) | count",
        "would hold more than 16777216 values",
    );
    answers("= @(1; 2; 3; 4; 5) | ... @| skipgrams(2; 1) @| count", "7");
}

/// Every forked value is a thread: a range or a direct product
/// sized by numbers in the query allocated with no bound.
#[test]
fn a_range_and_a_product_past_the_bound() {
    refused(
        "= (1..300000) + (1..300000) @| count",
        "the product of 300000 and 300000 values is past the 10000000",
    );
    refused(
        "= 1..9000000000 @| count",
        "holds 9000000000 values, past the 10000000",
    );
    refused(
        "= -9223372036854775808..9223372036854775807 @| count",
        "past the 10000000",
    );
    answers("= (1..300) + (1..300) @| count", "90000");
    answers("= 9223372036854775800..9223372036854775807 @| count", "8");
    answers("= 5..1 @| count", "5");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qua-never-panics-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn qua_in(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A title is data. The levels of a header nested a frame per colon,
/// and a title of 20,000 colons overflowed the stack. A title is one
/// name now, colons and all (ruling 2026-11-10); a header written
/// as a path nests to a bound, the rest of the name the innermost
/// key.
#[test]
fn a_title_of_many_colons() {
    let dir = scratch("colons");
    let title = format!("x{}", ":".repeat(20_000));
    std::fs::write(dir.join("colons.csv"), format!("{title},b\n1,2\n")).unwrap();
    let (code, out, err) = qua_in(&dir, &["/* @| # | json", "colons.csv"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, format!("[{{\"{title}\": 1, \"b\": 2}}]"));
    // a source's titles are names: no level is read off their colons
    std::fs::write(dir.join("dc.csv"), "dc:title,dc:creator,n\nIliad,Homer,1\n").unwrap();
    let (_, out, _) = qua_in(&dir, &["/* @| # | json", "dc.csv"]);
    assert_eq!(
        out,
        r#"[{"dc:title": "Iliad", "dc:creator": "Homer", "n": 1}]"#
    );
    // a header written as a path has levels, and a path of very many
    // nests to the bound
    let path = vec!["a"; 2_000].join(":");
    let (code, out, err) = qua_in(&dir, &[&format!("= #(@({path}; b); %(1; 2)) | json")]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.starts_with("[{\"a\": {\"a\": {"),
        "{}",
        &out[..40.min(out.len())]
    );
    let (_, out, _) = qua_in(
        &dir,
        &[r#"= #(@(dc:title; dc:creator; n); %("Iliad"; "Homer"; 1)) | json"#],
    );
    assert_eq!(
        out,
        r#"[{"dc": {"title": "Iliad", "creator": "Homer"}, "n": 1}]"#
    );
}

/// A span is data: `colspan="30000000"` pushed that many cells and
/// aborted on the allocation. Spans clamp as browsers clamp them.
#[test]
fn a_table_span_past_the_clamp() {
    let dir = scratch("span");
    std::fs::write(
        dir.join("span.html"),
        "<table><tr><th>a</th><th>b</th></tr>\
         <tr><td colspan=\"30000000\">x</td></tr>\
         <tr><td rowspan=\"4000000000\">y</td><td>z</td></tr></table>",
    )
    .unwrap();
    let (code, out, err) = qua_in(&dir, &["//table | # | dim", "span.html"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "@(2; 1000)");
}

/// A zero-weight edge at a seed gave the seed a predecessor, and the
/// walk back along the predecessors never ended, allocating until
/// memory was gone. A weight of zero is legal data (a free transfer).
#[test]
fn a_route_over_a_zero_weight_edge() {
    let dir = scratch("zero");
    std::fs::write(dir.join("nodes.csv"), "id\nA\nB\nC\n").unwrap();
    std::fs::write(dir.join("edges.csv"), "a,b,km\nA,B,0\nB,C,5\n").unwrap();
    std::fs::write(
        dir.join("z.model"),
        "mount nodes: nodes.csv;\nmount edges: edges.csv;\n\
         ref /edges/row::a --> /nodes/row[::id = $];\n\
         ref /edges/row::b --> /nodes/row[::id = $];\n\
         edge /edges/row: ::a -- ::b as e weights(::km<_km>);\n",
    )
    .unwrap();
    let (code, out, err) = qua_in(
        &dir,
        &[
            "--model",
            "z.model",
            "/nodes/row @| graph:route(--e; --from=/nodes/row[1] --weight=km) | count",
        ],
    );
    assert_eq!(code, Some(0), "{err}");
    // the seed alone, then one hop, then two
    assert_eq!(out, "1\n2\n3");
}

/// A clipboard tool that exits before it has read the text closes
/// the pipe; the write then killed qua by SIGPIPE (exit 141, no
/// message) and the next tool was never tried.
#[cfg(unix)]
#[test]
fn a_clipboard_tool_that_exits_early() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("clip");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    tool("xclip", "exit 1");
    tool(
        "xsel",
        &format!("cat > '{}'", dir.join("clip.out").display()),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .env_remove("WAYLAND_DISPLAY")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .arg("= \"x\" | repeat(240000) | copy")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("to xsel"), "{err}");
    assert_eq!(
        std::fs::metadata(dir.join("clip.out")).unwrap().len(),
        240_001
    );
}

/// A chain written flat is a tree as deep as it is long, and every
/// stage walked it by recursion: `1 + 1 + …` overflowed the stack at
/// 2,200 terms in a release build (150 in a debug one), a
/// conditional at 3,100 arms. The walkers grow their stack, and a
/// chain past the bound is refused.
#[test]
fn a_long_flat_chain() {
    let sum = |n: usize| format!("= {}", vec!["1"; n].join(" + "));
    answers(&sum(4000), "4000");
    refused(&sum(5001), "chains more than 5000 operands");
    let cond = |n: usize| format!("= ({}7)", "1 = 2 ? 1 : ".repeat(n));
    answers(&cond(1500), "7");
    refused(&cond(3001), "chains more than 3000 arms");
    // the unparser walks the same depth
    let (code, out, err) = qua_in(&std::env::temp_dir(), &["--expand", &sum(4000)]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.starts_with("= 1 + 1 + 1"),
        "{}",
        &out[..20.min(out.len())]
    );
}
