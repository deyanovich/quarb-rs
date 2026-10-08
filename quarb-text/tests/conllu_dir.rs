//! Finding 22: the treebank files beneath a directory decode under
//! the mount's rule (`?encoding=`, a byte-order mark, else strict
//! UTF-8), refused naming the file otherwise.

use quarb_text::{read_conllu_dir, read_conllu_dir_with};

#[test]
fn treebank_files_decode_under_the_mounts_rule() {
    let dir = std::env::temp_dir().join(format!("quarb-conllu-enc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // «Привет» in windows-1251, as a one-token sentence.
    let mut conllu = b"# text = \xcf\xf0\xe8\xe2\xe5\xf2\n1\t\xcf\xf0\xe8\xe2\xe5\xf2\t".to_vec();
    conllu.extend_from_slice(b"\xef\xf0\xe8\xe2\xe5\xf2\tINTJ\t_\t_\t0\troot\t_\t_\n\n");
    std::fs::write(dir.join("ru.conllu"), &conllu).unwrap();
    let err = read_conllu_dir(&dir).unwrap_err();
    assert!(
        err.contains("ru.conllu") && err.contains("?encoding="),
        "{err}"
    );
    let d = quarb::source::SourceDecoding::default()
        .with_option(Some("windows-1251"))
        .unwrap();
    let files = read_conllu_dir_with(&dir, &d).unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].1.contains("# text = Привет"), "{}", files[0].1);
}
