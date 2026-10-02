//! The statistics round (lang/TODO.md): standard scores, the sample
//! dispersion, the elementary and keyness scalars, the diversity
//! aggregates, and the association aggregates — each in the query,
//! not only in the stdlib's arithmetic.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"n":[2,4,6],
            "p":[{"x":1,"y":2},{"x":2,"y":4},{"x":3,"y":6}],
            "g":[{"k":"a","x":1,"y":2},{"k":"a","x":2,"y":4},
                 {"k":"b","x":1,"y":5},{"k":"b","x":2,"y":3}],
            "c":[1,1],
            "w":["a","a","b","b","c","c"],
            "parts":[{"n":4,"size":10},{"n":0,"size":10}],
            "z":[{"t":1.0,"c":0.0},{"t":0.0,"c":2.0}],
            "gold":[{"g":"per","p":"per","hit":true,"found":true},
                    {"g":"per","p":"loc","hit":true,"found":false},
                    {"g":"loc","p":"loc","hit":false,"found":true},
                    {"g":"loc","p":"loc","hit":false,"found":false}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

fn floats(q: &str) -> Vec<f64> {
    values(q)
        .iter()
        .map(|s| {
            s.parse::<f64>()
                .unwrap_or_else(|_| panic!("not a number: {s}"))
        })
        .collect()
}

fn close(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-9, "{a:?} vs {b:?}");
    }
}

#[test]
fn zscore_standardizes_the_context() {
    // 2, 4, 6: sample sd 2 → −1, 0, 1; population scores sum to 0
    // and agree with the arithmetic spelled out.
    close(&floats("/n/* | :: @| zscore(sample)"), &[-1.0, 0.0, 1.0]);
    let pop = floats("/n/* | :: @| zscore");
    assert!(pop.iter().sum::<f64>().abs() < 1e-12);
    let sd = (8.0f64 / 3.0).sqrt();
    close(&pop, &[-2.0 / sd, 0.0, 2.0 / sd]);
    // A partition key standardizes within each key.
    close(
        &floats("/g/* | ::x @| zscore(::k)"),
        &[-1.0, 1.0, -1.0, 1.0],
    );
    close(
        &floats("/g/* | ::x @| zscore(sample; ::k)"),
        &[
            -1.0 / 2f64.sqrt(),
            1.0 / 2f64.sqrt(),
            -1.0 / 2f64.sqrt(),
            1.0 / 2f64.sqrt(),
        ],
    );
    // No spread: null (printed empty) throughout.
    assert_eq!(values("/c/* | :: @| zscore"), ["", ""]);
    // It rides `@|` only.
    let e = quarb::run("/n/* | :: | zscore", &doc()).unwrap_err();
    assert!(e.to_string().contains("uses '@|'"), "{e}");
    let e = quarb::run("/n/* | :: @| zscore(2)", &doc()).unwrap_err();
    assert!(e.to_string().contains("sample"), "{e}");
}

#[test]
fn sample_dispersion() {
    close(&floats("/n/* | :: @| stddev(sample)"), &[2.0]);
    close(&floats("/n/* | :: @| stddev"), &[(8.0f64 / 3.0).sqrt()]);
    close(&floats("/n/* | :: @| variance(sample)"), &[4.0]);
    // Per group of records, with the ruling #66 projection.
    assert_eq!(
        values("/g/* | %(k = ::k; x = ::x) @| group(k = $_:k) | variance(:x; sample)"),
        ["0.5", "0.5"]
    );
    let e = quarb::run("/n/* | :: @| stddev(2)", &doc()).unwrap_err();
    assert!(e.to_string().contains("sample"), "{e}");
}

#[test]
fn elementary_and_keyness_scalars() {
    assert_eq!(values("/n/* | :: | pow(2)"), ["4", "16", "36"]);
    close(&floats("/n/* | :: | log(2)"), &[1.0, 2.0, 6f64.log2()]);
    close(&floats("/n/* | :: | sqrt | round(3)"), &[1.414, 2.0, 2.449]);
    assert_eq!(
        values("/n/* | %(e = (:: | exp | log | round(6)))")[0],
        "%(e = 2)"
    );
    // The expression form takes the base as the topic.
    assert_eq!(
        values("/n/* | %(p = pow(::; 2))"),
        ["%(p = 4)", "%(p = 16)", "%(p = 36)"]
    );
    // 10 of 1000 against 2 of 1000.
    assert_eq!(
        values("/n/*[:: = 2] | %(g = ((:: * 5) | loglik(1000; 2; 1000) | round(3)))"),
        ["%(g = 5.822)"]
    );
    // Over a record topic, the arguments are the record's fields —
    // the call form reads the enclosing capsa, not the piped value.
    assert_eq!(
        values(
            "/n/*[:: = 2] | %(a = :: * 5; na = 1000; b = 2; nb = 1000) \
             | %(g = (loglik(:a; :na; :b; :nb) | round(3)))"
        ),
        ["%(g = 5.822)"]
    );
    assert_eq!(
        values(r#"/p/* | %(s = "kitten"; t = "sitting") | %(d = levenshtein(:s; :t))"#)[0],
        "%(d = 3)"
    );
    assert_eq!(
        values("/n/*[:: = 2] | %(g = (loglik(:: * 5; 1000; 2; 1000) | round(3)))"),
        ["%(g = 5.822)"]
    );
    assert_eq!(
        values("/n/*[:: = 2] | %(x = ((:: * 5) | chi2(1000; 2; 1000) | round(2)))"),
        ["%(x = 5.37)"]
    );
}

#[test]
fn diversity_aggregates() {
    close(&floats("/c/* | :: @| entropy"), &[1.0]);
    close(&floats("/c/* | :: @| yule_k"), &[0.0]);
    close(&floats("/w/* | :: @| mtld(0.5)"), &[2.0]);
    // Over a grouped count table: counts are the group sizes.
    close(
        &floats("/w/* | :: @| group(w = $_) | count @| entropy"),
        &[3f64.log2()],
    );
}

#[test]
fn association_aggregates() {
    close(&floats("/p/* @| corr(::x; ::y)"), &[1.0]);
    close(
        &floats("/p/* | %(x = ::x; y = ::y) @| corr(:x; :y)"),
        &[1.0],
    );
    close(&floats("/p/* @| spearman(::x; ::y)"), &[1.0]);
    close(&floats("/p/* @| cosine(::x; ::y)"), &[1.0]);
    // Per group on the plain pipe: a rises, b falls.
    close(
        &floats("/g/* @| group(k = ::k) | corr(::x; ::y)"),
        &[1.0, -1.0],
    );
    close(
        &floats("/g/* | %(k = ::k; x = ::x; y = ::y) @| group(k = $_:k) | corr(:x; :y)"),
        &[1.0, -1.0],
    );
    let e = quarb::run("/p/* @| corr(::x)", &doc()).unwrap_err();
    assert!(e.to_string().contains("two"), "{e}");
}

#[test]
fn collocation_scalars() {
    // A pair of twenty in a million tokens, words of a hundred and
    // two hundred: MI = log2(1000); the pipe form takes the pair's
    // count as the value, the expression form all four.
    close(
        &floats("/n/*[:: = 2] | :: * 10 | mi(100; 200; 1000000)"),
        &[1000f64.log2()],
    );
    close(
        &floats("/n/*[:: = 2] | %(m = mi(:: * 10; 100; 200; 1000000)) | :m"),
        &[1000f64.log2()],
    );
    close(
        &floats("/n/*[:: = 2] | :: * 10 | t_score(100; 200; 1000000)"),
        &[(20.0 - 0.02) / 20f64.sqrt()],
    );
    close(&floats("/n/*[:: = 2] | :: | log_dice(2; 2)"), &[14.0]);
    assert_eq!(values("/n/*[:: = 2] | :: | mi(0; 200; 1000000)"), [""]);
}

#[test]
fn distance_dispersion_and_agreement() {
    close(&floats("/z/* @| delta(::t; ::c)"), &[1.5]);
    close(&floats("/parts/* @| dp(::n; ::size)"), &[0.5]);
    // κ over labels, precision and recall over truthiness, and the
    // per-group form on the plain pipe.
    close(&floats("/gold/* @| kappa(::g; ::p)"), &[0.5]);
    close(&floats("/gold/* @| precision(::hit; ::found)"), &[0.5]);
    close(&floats("/gold/* @| recall(::hit; ::found)"), &[0.5]);
    close(&floats("/gold/* @| f1(::hit; ::found)"), &[0.5]);
    // The "per" group agrees on half its pairs with a chance
    // agreement of a half: κ = 0; the "loc" group names one category
    // on both sides: null.
    assert_eq!(values("/gold/* @| group(::g) | kappa(::g; ::p)"), ["0", ""]);
}
