//! A dump of an HWP 5 file's streams and records, for checking the
//! reader against real files: `HWP_PROBE=path cargo test --test
//! probe -- --nocapture`. Skipped otherwise.
#[test]
fn probe() {
    let Ok(path) = std::env::var("HWP_PROBE") else {
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    println!("streams: {:?}", quarb_text_hwpx::hwp5_streams(&bytes));
    println!("{}", quarb_text_hwpx::hwp5_dump(&bytes));
}
