//! The DjVu reading over a document built in code: three pages
//! with text layers (the third empty), an outline of two chapters
//! and a nested note, and ANT metadata.

use djvu_rs::DjVuBookmark;
use djvu_rs::chunk_encode::{ChunkEncoder, InfoChunk, NavmChunk, TextChunk};
use djvu_rs::djvm::{DjvmSpool, DjvmStreamWriter};
use djvu_rs::iff::{self, Chunk, DjvuFile};
use djvu_rs::text::{Rect, TextLayer, TextZone, TextZoneKind};
use quarb::QueryResult;

fn zone(
    kind: TextZoneKind,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    text: &str,
    children: Vec<TextZone>,
) -> TextZone {
    TextZone {
        kind,
        rect: Rect {
            x,
            y,
            width: w,
            height: h,
        },
        text: text.to_string(),
        children,
    }
}

fn line(y: u32, words: &[&str]) -> TextZone {
    let mut x = 100;
    let mut kids = Vec::new();
    for w in words {
        kids.push(zone(TextZoneKind::Word, x, y, 80, 30, w, vec![]));
        x += 100;
    }
    zone(
        TextZoneKind::Line,
        100,
        y,
        x - 100,
        30,
        &words.join(" "),
        kids,
    )
}

/// One page: paragraphs of lines of words, in one column.
fn page(paras: &[&[&[&str]]]) -> Vec<u8> {
    let (width, height) = (800u16, 1200u16);
    let mut y = 100;
    let mut para_zones = Vec::new();
    let mut full = String::new();
    for lines in paras {
        let mut lz = Vec::new();
        let y0 = y;
        for words in lines.iter() {
            lz.push(line(y, words));
            full.push_str(&words.join(" "));
            full.push('\n');
            y += 40;
        }
        para_zones.push(zone(TextZoneKind::Para, 100, y0, 600, y - y0, "", lz));
        y += 40;
    }
    let page = zone(
        TextZoneKind::Page,
        0,
        0,
        width as u32,
        height as u32,
        &full,
        vec![zone(
            TextZoneKind::Column,
            50,
            50,
            700,
            1100,
            "",
            para_zones,
        )],
    );
    let layer = TextLayer {
        text: full,
        zones: vec![page],
    };
    let info = InfoChunk {
        width,
        height,
        dpi: 300,
    }
    .encode_chunk()
    .unwrap()
    .into_leaf();
    // A page without text carries no text chunk at all.
    let mut children = vec![info];
    if !paras.is_empty() {
        children.push(
            TextChunk {
                layer: &layer,
                page_height: height as u32,
            }
            .encode_chunk()
            .unwrap()
            .into_leaf(),
        );
    }
    iff::emit(&DjvuFile {
        root: Chunk::Form {
            secondary_id: *b"DJVU",
            length: 0,
            children,
        },
    })
}

pub fn onegin() -> Vec<u8> {
    let p1 = page(&[
        &[&["ГЛАВА", "ПЕРВАЯ"]],
        &[
            &["Мой", "дядя", "самых", "честных", "правил,"],
            &["Когда", "не", "в", "шутку", "занемог,"],
        ],
    ]);
    let p2 = page(&[
        &[&["ГЛАВА", "ВТОРАЯ"]],
        &[&["Деревня,", "где", "скучал", "Евгений,"]],
    ]);
    let p3 = page(&[]);
    let navm = NavmChunk(&[
        DjVuBookmark {
            title: "Глава первая".into(),
            url: "#1".into(),
            children: vec![],
        },
        DjVuBookmark {
            title: "Глава вторая".into(),
            url: "#p2.djvu".into(),
            children: vec![DjVuBookmark {
                title: "Примечания".into(),
                url: "#3".into(),
                children: vec![],
            }],
        },
    ])
    .encode_chunk()
    .unwrap();
    let meta = djvu_rs::metadata::DjVuMetadata {
        title: Some("Евгений Онегин".into()),
        author: Some("Пушкин".into()),
        year: Some("1837".into()),
        ..Default::default()
    };
    let ant = djvu_rs::metadata::encode_metadata(&meta);
    let mut w = DjvmStreamWriter::new(Vec::new(), DjvmSpool::Memory).unwrap();
    w.add_component("p1.djvu", 1, &p1).unwrap();
    w.add_component("p2.djvu", 1, &p2).unwrap();
    w.add_component("p3.djvu", 1, &p3).unwrap();
    w.add_document_chunk(navm.id, &navm.payload).unwrap();
    w.add_document_chunk(*b"ANTa", &ant).unwrap();
    w.finish().unwrap()
}

fn values(model: &quarb_text_djvu::DjvuText, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

#[test]
fn sections_pages_zones_and_geometry() {
    let m = quarb_text_djvu::parse(&onegin()).unwrap();
    // the outline is the heading ladder; a section holds its pages
    assert_eq!(
        values(&m, "/section::lemma"),
        ["Глава первая", "Глава вторая"]
    );
    assert_eq!(values(&m, "/section/section::lemma"), ["Примечания"]);
    assert_eq!(values(&m, "/section[1]/page::::page"), ["1"]);
    assert_eq!(values(&m, "/section[2]/page::::page"), ["2"]);
    assert_eq!(values(&m, "/section/section/page::::page"), ["3"]);
    // zones as the layer names them; words are a line's text
    assert_eq!(values(&m, "//column @| count"), ["2"]);
    assert_eq!(values(&m, "//paragraph @| count"), ["4"]);
    assert_eq!(values(&m, "//line @| count"), ["5"]);
    assert_eq!(values(&m, "//word @| count"), ["0"]);
    assert_eq!(
        values(&m, "//line[2]::"),
        ["Мой дядя самых честных правил,"]
    );
    assert_eq!(
        values(&m, "//paragraph[2]::"),
        ["Мой дядя самых честных правил,\nКогда не в шутку занемог,"]
    );
    // geometry on every zone, in page pixels
    assert_eq!(
        values(
            &m,
            "//line[2] | %(::::page; ::::x; ::::y; ::::width; ::::height)"
        ),
        ["%(page = 1; x = 100; y = 180; width = 500; height = 30)"]
    );
    assert_eq!(values(&m, "//paragraph[2]::::height"), ["80"]);
    // a page without a text layer has no zones and still mounts
    assert_eq!(values(&m, "//page @| count"), ["3"]);
    assert_eq!(values(&m, "//page[3]/* @| count"), ["0"]);
    assert_eq!(values(&m, "//page[3]::::text-layer"), ["false"]);
    assert_eq!(values(&m, "^::::text-layer"), ["true"]);
    assert_eq!(values(&m, "^::::n-pages"), ["3"]);
    assert_eq!(
        values(&m, "//page[1] | %(::::width; ::::height; ::::dpi)"),
        ["%(width = 800; height = 1200; dpi = 300)"]
    );
    // the declared identity
    assert_eq!(
        values(&m, "^ | %(::title; ::author; ::year)"),
        ["%(title = \"Евгений Онегин\"; author = \"Пушкин\"; year = \"1837\")"]
    );
}

#[test]
fn without_an_outline_the_tree_is_pages() {
    let bytes = page(&[&[&["Одна", "строка"]]]);
    let m = quarb_text_djvu::parse(&bytes).unwrap();
    assert_eq!(values(&m, "/* | :::name"), ["page"]);
    assert_eq!(values(&m, "/page/column/paragraph/line::"), ["Одна строка"]);
    assert_eq!(values(&m, "^::::text-layer"), ["true"]);
    assert!(quarb_text_djvu::parse(b"not a djvu").is_err());
}
