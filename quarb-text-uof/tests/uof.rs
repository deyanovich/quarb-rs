//! A UOF text at the text level: headings from the outline level
//! (declared on the paragraph or through its style), lists from the
//! automatic numberings, tables with their header rows, notes,
//! hyperlinks through the link set, bookmarks through the bookmark
//! set, the accepted view of tracked changes, the metadata. The
//! documents are written in-test after the UOF 1.0 vocabulary as
//! LibreOffice's import reads it.
use std::io::Write;

use quarb::{AstAdapter, QueryResult, Value};

const NS: &str = r#"xmlns:uof="http://schemas.uof.org/cn/2003/uof" xmlns:字="http://schemas.uof.org/cn/2003/uof-wordproc" xmlns:表="http://schemas.uof.org/cn/2003/uof-spreadsheet" xmlns:演="http://schemas.uof.org/cn/2003/uof-slideshow""#;

fn run(text: &str) -> String {
    format!("<字:句><字:文本串>{text}</字:文本串></字:句>")
}

fn p(props: &str, runs: &str) -> String {
    format!("<字:段落>{props}{runs}</字:段落>")
}

fn emma() -> String {
    let body = [
        p("<字:段落属性><字:大纲级别>1</字:大纲级别></字:段落属性>", &run("Volume I")),
        p("<字:段落属性 字:式样引用=\"h2\"/>", &run("Chapter I")),
        p("", &format!("{}{}", run("Emma Woodhouse, handsome, clever, and rich,"), run(" had lived nearly twenty-one years."))),
        p("<字:段落属性><字:自动编号信息 字:编号引用=\"bul\"/></字:段落属性>", &run("Her father")),
        p("<字:段落属性><字:自动编号信息 字:编号引用=\"bul\" 字:级别=\"1\"/></字:段落属性>", &run("affectionate")),
        p("<字:段落属性><字:自动编号信息 字:编号引用=\"bul\"/></字:段落属性>", &run("Her governess")),
        p("<字:段落属性><字:自动编号信息 字:编号引用=\"num\"/></字:段落属性>", &run("First")),
        p("<字:段落属性><字:自动编号信息 字:编号引用=\"num\"/></字:段落属性>", &run("Second")),
        "<字:文字表><字:行><字:表行属性><字:表头行 字:值=\"true\"/></字:表行属性><字:单元格><字:段落><字:句><字:文本串>title</字:文本串></字:句></字:段落></字:单元格><字:单元格><字:段落><字:句><字:文本串>price</字:文本串></字:句></字:段落></字:单元格></字:行><字:行><字:单元格><字:段落><字:句><字:文本串>Emma</字:文本串></字:句></字:段落></字:单元格><字:单元格><字:段落><字:句><字:文本串>25</字:文本串></字:句></字:段落></字:单元格></字:行></字:文字表>".to_string(),
        p("", "<字:句><字:文本串>Highbury</字:文本串><字:脚注 字:引文体=\"1\"><字:段落><字:句><字:文本串>A large and populous village.</字:文本串></字:句></字:段落></字:脚注><字:文本串> is a village.</字:文本串></字:句>"),
        p("", "<字:句><字:文本串>See </字:文本串><字:区域开始 字:类型=\"hyperlink\" 字:标识符=\"L1\"/><字:文本串>Read it</字:文本串><字:区域结束 字:标识符=\"L1\"/><字:文本串> online.</字:文本串></字:句>"),
        p("", "<字:句><字:区域开始 字:类型=\"bookmark\" 字:标识符=\"B1\"/><字:文本串>Mr. </字:文本串><字:修订开始 字:类型=\"delete\" 字:标识符=\"d1\"/><字:文本串>Woodhouse</字:文本串><字:修订结束 字:标识符=\"d1\"/><字:修订开始 字:类型=\"insert\" 字:标识符=\"i1\"/><字:文本串>Knightley</字:文本串><字:修订结束 字:标识符=\"i1\"/><字:文本串> called.</字:文本串><字:区域结束 字:标识符=\"B1\"/></字:句>"),
        p("<字:段落属性><字:大纲级别>2</字:大纲级别></字:段落属性>", &run("Chapter II")),
        p("", &run("Mr. Weston was a native of Highbury.")),
    ]
    .concat();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><uof:UOF {NS} uof:language="en" uof:version="1.0" uof:mimetype="vnd.uof.text"><uof:元数据><uof:标题>Emma</uof:标题><uof:创建者>Jane Austen</uof:创建者><uof:主题>A novel</uof:主题><uof:关键字集><uof:关键字>Highbury</uof:关键字><uof:关键字>manners</uof:关键字></uof:关键字集><uof:创建日期>1815-12-23T00:00:00Z</uof:创建日期></uof:元数据><uof:式样集><uof:段落式样 字:标识符="h2" 字:名称="Heading 2"><字:大纲级别>2</字:大纲级别></uof:段落式样><uof:自动编号集><字:自动编号 字:标识符="bul"><字:级别 字:级别值="1"><字:项目符号 字:字符="•"/></字:级别></字:自动编号><字:自动编号 字:标识符="num"><字:级别 字:级别值="1"><字:编号格式>decimal</字:编号格式></字:级别></字:自动编号></uof:自动编号集></uof:式样集><uof:链接集><uof:超级链接 uof:链源="L1" uof:链宿="https://www.gutenberg.org/ebooks/158" uof:提示="Gutenberg"/></uof:链接集><uof:书签集><uof:书签 uof:名称="knightley"><uof:文本位置 字:区域引用="B1"/></uof:书签></uof:书签集><uof:文字处理><字:公用处理规则/><字:主体>{body}</字:主体></uof:文字处理></uof:UOF>"#
    )
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

#[test]
fn headings_declared_on_the_paragraph_or_through_its_style() {
    let m = quarb_text_uof::parse(emma().as_bytes()).unwrap();
    assert_eq!(values("/section::lemma", &m), ["Volume I"]);
    assert_eq!(
        values("/section/section::lemma", &m),
        ["Chapter I", "Chapter II"]
    );
    assert_eq!(
        values("/section/section[1]/paragraph[1]::", &m),
        ["Emma Woodhouse, handsome, clever, and rich, had lived nearly twenty-one years."]
    );
}

#[test]
fn lists_tables_notes_links_bookmarks_and_the_accepted_view() {
    let m = quarb_text_uof::parse(emma().as_bytes()).unwrap();
    assert_eq!(
        values("/section/section[1]/unordered-list/unordered-item::", &m),
        ["Her father\naffectionate", "Her governess"]
    );
    assert_eq!(
        values("/section/section[1]/ordered-list[1]/ordered-item::", &m),
        ["First", "Second"]
    );
    assert_eq!(
        values("//*<tabula>//*<cell> | ::lemma", &m),
        ["title", "price"]
    );
    assert_eq!(values("//*<tabula>//*<cell> | ::body", &m), ["Emma", "25"]);
    assert_eq!(
        values("//footnote<note>::", &m),
        ["A large and populous village."]
    );
    assert_eq!(
        values("/section/section[1]/paragraph[2]::", &m),
        ["Highbury is a village."]
    );
    assert_eq!(
        values("//ref | %(::target; t = ::)", &m),
        ["%(target = \"https://www.gutenberg.org/ebooks/158\"; t = \"Read it\")"]
    );
    assert_eq!(values("//anchor::onym", &m), ["knightley"]);
    assert_eq!(
        values("/section/section[1]/paragraph[4]::", &m),
        ["Mr. Knightley called."]
    );
}

#[test]
fn the_metadata_is_the_identity_and_a_package_reads_too() {
    let h = quarb_text_uof::head(emma().as_bytes()).unwrap();
    assert_eq!(h.title.as_deref(), Some("Emma"));
    assert_eq!(h.author.as_deref(), Some("Jane Austen"));
    assert_eq!(h.category.as_deref(), Some("A novel"));
    assert_eq!(h.tags, ["Highbury", "manners"]);
    assert!(h.published.is_some());
    // A UOF 2.0 package: the same document as a zip member.
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    w.start_file("mimetype", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.write_all(b"vnd.uof.text").unwrap();
    w.start_file("uof.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.write_all(emma().as_bytes()).unwrap();
    let pkg = w.finish().unwrap().into_inner();
    assert!(quarb_text_uof::is_uof(&pkg));
    let m = quarb_text_uof::parse(&pkg).unwrap();
    assert_eq!(values("/section::lemma", &m), ["Volume I"]);
    assert!(!quarb_text_uof::is_uof(b"<root/>"));
    let err = match quarb_text_uof::parse(b"<?xml version=\"1.0\"?><root/>") {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a non-UOF root parsed"),
    };
    assert!(err.contains("not uof:UOF"), "{err}");
}
