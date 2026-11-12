//! Hash (d) of the columnar round (2026-11-06): the lines of a large
//! file under `lines:` are streamed under qua — the line starts and
//! the blank ones from one pass, a line's text read as a step reaches
//! it. The answers are the whole reading's (`--no-stream`).
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir =
        std::env::temp_dir().join(format!("quarb-stream-lines-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let words = [
        "Sing", "goddess", "the", "wrath", "of", "Achilles", "son", "Peleus",
    ];
    let mut out = Vec::with_capacity(40000);
    for i in 0..40000usize {
        if i % 9 == 0 {
            out.push(String::new());
        } else if i % 13 == 0 {
            out.push("   ".to_string());
        } else {
            let n = 2 + (i * 7) % 8;
            out.push(
                (0..n)
                    .map(|k| words[(i * 3 + k * 5) % words.len()])
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    std::fs::write(dir.join("big.txt"), out.join("\n") + "\n").unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

#[test]
fn the_streamed_lines_answer_as_the_whole_reading() {
    let d = fixture();
    for q in [
        "//line @| count",
        "//line<!blank> @| count",
        "/line[::blank] @| count",
        "/line[..3] | ::",
        "/line[12] | ::",
        "/line[12]:::n",
        "/line[::n = 12] | ::",
        "/line[..5][::blank] @| count",
        "/line[::n = 39999] | ::",
        "/line[::n > 39990] | ::n",
        // lines read out of the file's order
        "/line @| reverse @| [..3] | ::",
        "/line<!blank> @| sort_by(::) @| [..3] | ::n",
    ] {
        let (a, ea, ga) = qua(&d, &[q, "lines:big.txt"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "lines:big.txt"]);
        assert_eq!(ga, gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
    }
    assert_eq!(qua(&d, &["//line @| count", "lines:big.txt"]).0, "40000");
}
