//! The columnar round's fast paths (2026-11-06): a keyed command
//! (`sort_by`, `top`, `bottom`, `unique_by`, `min_by`, `max_by`) over a
//! table whose every key is a bare column read ranks by the columns
//! themselves, and a `--of=:col` reduction reads the column — no
//! record per row. The answers are the record path's, forced here by
//! the non-bare spelling `$_:col`, which the fast paths do not take.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-colfast-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("u.csv"),
        "Name,Age,Fare,Note\nBraund,22,7.25,x\nCumings,,71.5,y\nHeikkinen,26,8.75,x\nFutrelle,35,53.1,\nAllen,26,8.05,y\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, q: &str) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args([q, "u.csv"])
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

#[test]
fn the_column_paths_answer_as_the_record_path() {
    let d = fixture();
    for (fast, slow) in [
        ("/row @| # | sort_by(:Age)", "/row @| # | sort_by($_:Age)"),
        (
            "/row @| # | sort_by(:Age; :Fare)",
            "/row @| # | sort_by($_:Age; $_:Fare)",
        ),
        (
            "/row @| # | sort_by(:Note; :Fare)",
            "/row @| # | sort_by($_:Note; $_:Fare)",
        ),
        (
            "/row @| # | sort_by(:Age; --desc)",
            "/row @| # | sort_by($_:Age; --desc)",
        ),
        ("/row @| # | top(2; :Fare)", "/row @| # | top(2; $_:Fare)"),
        (
            "/row @| # | bottom(2; :Age)",
            "/row @| # | bottom(2; $_:Age)",
        ),
        (
            "/row @| # | unique_by(:Age)",
            "/row @| # | unique_by($_:Age)",
        ),
        ("/row @| # | min_by(:Age)", "/row @| # | min_by($_:Age)"),
        ("/row @| # | max_by(:Fare)", "/row @| # | max_by($_:Fare)"),
        (
            "/row @| # | mean(--of=:Age)",
            "/row @| # | mean(--of=$_:Age)",
        ),
        (
            "/row @| # | sum(--of=:Fare)",
            "/row @| # | sum(--of=$_:Fare)",
        ),
        (
            "/row @| # | count(--of=:Note)",
            "/row @| # | count(--of=$_:Note)",
        ),
        (
            "/row @| # | unique(--of=:Note)",
            "/row @| # | unique(--of=$_:Note)",
        ),
        (
            "/row @| #(-i=:Name) | sort_by(:::index) | [..2]",
            "/row @| #(-i=:Name) | sort_by($_:Name) | [..2]",
        ),
        (
            "/row @| #(-i=:Name) | top(1; :Fare)",
            "/row @| #(-i=:Name) | top(1; $_:Fare)",
        ),
        ("/row @| # | sort_by(:Nope)", "/row @| # | sort_by($_:Nope)"),
        (
            "/row @| # | mean(--of=:Nope)",
            "/row @| # | mean(--of=$_:Nope)",
        ),
    ] {
        let (a, ea, ga) = qua(&d, fast);
        let (b, eb, gb) = qua(&d, slow);
        assert_eq!(ga, gb, "{fast}: {ea} / {eb}");
        assert_eq!(a, b, "{fast}");
    }
    // the null sorts last, the ties keep their order
    let (out, err, good) = qua(&d, "/row @| # | sort_by(:Age) | :Name");
    assert!(good, "{err}");
    assert_eq!(
        out,
        "@(\"Braund\"; \"Heikkinen\"; \"Allen\"; \"Futrelle\"; \"Cumings\")"
    );
    let (out, _, _) = qua(&d, "/row @| # | mean(--of=:Age)");
    assert_eq!(out, "27.25");
}
