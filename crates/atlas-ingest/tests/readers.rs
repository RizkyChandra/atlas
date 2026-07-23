//! Integration tests for the M8 document readers, over small fixtures.
//! Markdown / CSV / ipynb use static fixtures under `tests/fixtures/`; the
//! binary formats (pdf/docx/xlsx) generate a tiny fixture at test time and
//! round-trip it back to non-empty text.

use atlas_ingest::{chunk_text, csv_reader, ipynb, markdown};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn attr<'a>(m: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> &'a str {
    m.get(k).and_then(|v| v.as_str()).unwrap_or_default()
}

#[test]
fn markdown_emits_doc_node_and_reference_edges() {
    let ex = markdown::extract_markdown(fixture("doc.md"));

    // Doc node for the file itself.
    let doc = &ex.nodes[0];
    assert_eq!(attr(doc, "file_type"), "document");
    assert_eq!(attr(doc, "label"), "doc.md");

    // Exactly two `references` edges: the [x](./y.md) link and the [[wiki]]
    // link. The external URL and the image must be skipped.
    let refs: Vec<&_> = ex
        .links_with_relation("references")
        .collect();
    assert_eq!(refs.len(), 2, "only sibling-doc links become references");
    let targets: Vec<&str> = refs.iter().map(|e| attr(e, "target")).collect();
    assert!(targets.iter().any(|t| t.ends_with("y_md")), "targets: {targets:?}");
    assert!(targets.iter().any(|t| t.ends_with("wiki_md")), "targets: {targets:?}");

    // Headings become nodes with `contains` edges.
    assert!(ex.nodes.iter().any(|n| attr(n, "label") == "Subsection"));
    assert!(ex.links_with_relation("contains").count() >= 1);
}

#[test]
fn csv_emits_doc_and_capped_columns() {
    let ex = csv_reader::extract_csv(fixture("small.csv"), 2);
    // 1 doc node + 3 column nodes.
    assert_eq!(ex.nodes.len(), 4);
    let doc = &ex.nodes[0];
    assert_eq!(attr(doc, "file_type"), "document");
    assert_eq!(doc["column_count"], 3);
    assert_eq!(doc["row_count"], 2, "row scan capped at 2");
    assert_eq!(doc["rows_truncated"], true);
    // One `contains` edge per column.
    assert_eq!(ex.edges.len(), 3);
    let cols: Vec<&str> = ex.nodes[1..].iter().map(|n| attr(n, "label")).collect();
    assert_eq!(cols, ["name", "age", "city"]);
}

#[test]
fn ipynb_concatenates_markdown_and_code() {
    let text = ipynb::ipynb_to_text(fixture("notebook.ipynb"));
    assert!(!text.is_empty());
    assert!(text.contains("Notebook Title"));
    assert!(text.contains("print('hi from a code cell')"));
}

#[test]
fn chunk_text_covers_and_bounds() {
    let doc = "# A\n\n".to_string() + &"word ".repeat(200);
    let chunks = chunk_text(&doc, 20);
    assert!(chunks.len() > 1);
    for c in &chunks {
        assert!(atlas_ingest::chunk::estimate_tokens(&c.text) <= 20);
    }
    let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(rejoined, doc);
}

// ── binary formats: generate a tiny fixture, round-trip it ──────────────────

#[cfg(feature = "pdf")]
#[test]
fn pdf_round_trip_non_empty() {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let path = std::env::temp_dir().join(format!("atlas_it_{}.pdf", std::process::id()));
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 24.into()]),
            Operation::new("Td", vec![100.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal("Hello Atlas PDF")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => resources,
    });
    let pages = dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc.save(&path).unwrap();

    let text = atlas_ingest::pdf::pdf_to_text(&path);
    let _ = std::fs::remove_file(&path);
    assert!(!text.trim().is_empty(), "pdf text should be non-empty");
    assert!(text.contains("Hello"), "extracted: {text:?}");
}

#[cfg(feature = "office")]
#[test]
fn docx_round_trip_non_empty() {
    let path = std::env::temp_dir().join(format!("atlas_it_{}.docx", std::process::id()));
    let doc_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:r><w:t>Hello from a docx paragraph</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Second paragraph line</w:t></w:r></w:p>
</w:body></w:document>"#;
    write_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES_DOCX),
            ("_rels/.rels", RELS_DOCX),
            ("word/document.xml", doc_xml),
        ],
    );

    let text = atlas_ingest::office::docx_to_text(&path);
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("Hello from a docx paragraph"), "extracted: {text:?}");
    assert!(text.contains("Second paragraph line"));
}

#[cfg(feature = "office")]
#[test]
fn xlsx_round_trip_non_empty() {
    let path = std::env::temp_dir().join(format!("atlas_it_{}.xlsx", std::process::id()));
    write_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES_XLSX),
            ("_rels/.rels", RELS_XLSX),
            ("xl/workbook.xml", WORKBOOK_XLSX),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS_XLSX),
            ("xl/worksheets/sheet1.xml", SHEET1_XLSX),
        ],
    );

    let text = atlas_ingest::office::xlsx_to_text(&path);
    let _ = std::fs::remove_file(&path);
    assert!(!text.trim().is_empty(), "xlsx text should be non-empty");
    assert!(text.contains("product"), "extracted: {text:?}");
    assert!(text.contains("widget"));
}

#[cfg(feature = "office")]
fn write_zip(path: &std::path::Path, entries: &[(&str, &str)]) {
    use std::io::Write;
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[cfg(feature = "office")]
const CONTENT_TYPES_DOCX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

#[cfg(feature = "office")]
const RELS_DOCX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

#[cfg(feature = "office")]
const CONTENT_TYPES_XLSX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;

#[cfg(feature = "office")]
const RELS_XLSX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

#[cfg(feature = "office")]
const WORKBOOK_XLSX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;

#[cfg(feature = "office")]
const WORKBOOK_RELS_XLSX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

#[cfg(feature = "office")]
const SHEET1_XLSX: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>product</t></is></c><c r="B1" t="inlineStr"><is><t>qty</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>widget</t></is></c><c r="B2"><v>42</v></c></row>
</sheetData>
</worksheet>"#;
