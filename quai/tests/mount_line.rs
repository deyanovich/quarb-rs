//! The mount line (ruling 2026-11-12): `mount NAME: ?options`
//! re-reads a source already mounted under the options a mount
//! writes after `?`. A line of the session, and a model cell of a
//! notebook, in every host; the browser's session, which cannot
//! write the options on a target, has no other door to them.
use std::io::Write;
use std::process::{Command, Stdio};

fn dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quai-mount-line-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("grain.csv"),
        "port,tons\nPiraeus,NA\nKoper,\"1,5\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("ports.json"), r#"{"n": 2}"#).unwrap();
    dir
}

fn quai(dir: &std::path::Path, args: &[&str], lines: &str) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quai"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("QUARB_LOCALE", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A notebook: the cell re-reads the source, takes its ordinal and
/// prints nothing; the cells after it read the source as it now is.
#[test]
fn a_notebook_cell_re_reads_a_source_under_options() {
    let d = dir("nb");
    std::fs::write(
        d.join("nb.quai"),
        "    mount grain: grain.csv;\n\n    /grain/row | ::tons\n\n    mount grain: ?null=NA&decimal=comma;\n\n    \
         /grain/row | ::tons\n\n    mount: ?;\n\n    /grain/row[1] | ::tons\n\n    mount grain: ?nulls=NA;\n\n    \
         /grain/row[1] | ::tons\n\n    mount wheat: ?null=NA;\n",
    )
    .unwrap();
    let (out, err) = quai(&d, &["--run", "nb.quai"], "");
    let want = "&1  mount grain: grain.csv;\n\n\
                &2  /grain/row | ::tons\n    NA\n    1,5\n\n\
                &3  mount grain: ?null=NA&decimal=comma;\n\n\
                &4  /grain/row | ::tons\n    null\n    1.5\n\n\
                &5  mount: ?;\n\n\
                &6  /grain/row[1] | ::tons\n    NA\n\n\
                &7  mount grain: ?nulls=NA;\n";
    assert!(out.starts_with(want), "{out}\n{err}");
    // the refused re-read names what the source takes, and the
    // source is as it was; a name that mounts nothing is told
    assert!(out.contains("unknown option \"nulls=NA\""), "{out}");
    assert!(
        out.contains("&8  /grain/row[1] | ::tons\n    NA\n"),
        "{out}"
    );
    assert!(
        out.contains("&9  mount wheat: ?null=NA;\n    error: no source is mounted as 'wheat'; the sources: grain"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A line of the interactive session: the same statement, with or
/// without its semicolon; it takes its ordinal, and a refused one
/// takes none.
#[test]
fn a_line_of_the_session_re_reads_a_source() {
    let d = dir("line");
    let lines = "/row | ::tons\n\
                 mount: ?null=NA&decimal=comma\n\
                 /row | ::tons\n\
                 &1\n\
                 mount grain: ?nulls=NA\n\
                 /row @| count\n\
                 :history\n";
    let (out, err) = quai(&d, &["grain.csv"], lines);
    assert!(out.contains("NA\n1,5\n"), "{out}\n{err}");
    // after the re-read, the new reading and the earlier line again
    assert_eq!(out.matches("null\n1.5\n").count(), 2, "{out}\n{err}");
    assert!(err.contains("unknown option \"nulls=NA\""), "{err}");
    // the history: the mount line took &2 (it is no fragment, so
    // no def stands for it), the refused one nothing
    assert!(out.contains("def &3: /row | ::tons ;"), "{out}");
    assert!(!out.contains("def &2:"), "{out}");
    assert!(out.contains("def &5: /row @| count ;"), "{out}");
    // several sources: the name is asked for, and it is honoured
    let (out, err) = quai(
        &d,
        &["grain.csv", "ports.json"],
        "mount: ?null=NA\nmount grain: ?null=NA\n/grain/row[1] | ::tons\n",
    );
    assert!(
        err.contains(
            "name the source to re-read, 'mount NAME: ?null=NA'; the sources: grain, ports"
        ),
        "{err}"
    );
    assert!(out.contains("null"), "{out}\n{err}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A file a mount's option names (`sentences=`, `conllu=`, `cast=`,
/// `places=`, `desm=`) is found where the mount's own target is:
/// beside the notebook. It was read against the working directory,
/// so a notebook run from elsewhere failed its mount and the cells
/// after it answered nothing.
#[test]
fn an_options_file_is_found_beside_the_notebook() {
    let d = dir("optfile");
    let nb = d.join("book");
    std::fs::create_dir_all(&nb).unwrap();
    std::fs::write(
        nb.join("play.md"),
        "# The Play\n\nA word here. Another\none!\n\n## Act Two\n\nLast words.\n",
    )
    .unwrap();
    std::fs::write(
        nb.join("play.sentences"),
        "The Play\nA word here. Another one!\n\nAct Two\nLast words.\n",
    )
    .unwrap();
    std::fs::write(
        nb.join("nb.quai"),
        "    mount play: corpus:play.md?sentences=play.sentences;\n\n    \
         /play//sentence @| count\n\n    mount play: ?sentences=play.sentences&notes=omit;\n\n    \
         /play//sentence[:::n = 2] | ::\n",
    )
    .unwrap();
    // run from the parent directory: neither file is in it
    let (out, err) = quai(&d, &["--run", "book/nb.quai"], "");
    let want = "&1  mount play: corpus:play.md?sentences=play.sentences;\n\n\
                &2  /play//sentence @| count\n    4\n\n\
                &3  mount play: ?sentences=play.sentences&notes=omit;\n\n\
                &4  /play//sentence[:::n = 2] | ::\n    A word here. Another one!\n";
    assert!(out.starts_with(want), "{out}\n{err}");
    // and from the notebook's own directory, as before
    let (out, err) = quai(&nb, &["--run", "nb.quai"], "");
    assert!(out.starts_with(want), "{out}\n{err}");
}
