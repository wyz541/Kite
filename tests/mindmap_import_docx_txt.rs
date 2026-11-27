use std::io::{Cursor, Write};
use std::path::Path;

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_import::{
    ContentKind, ConversionOptions, ConversionPreset, ConversionWarningCode, DocxAdapter,
    DocxAdapterOptions, OutlineAdapter, OutlineDocument, OutlineNode, OutlineNodeKind,
    SourceFormat, SourceInput, TextAdapter, TextAdapterOptions, TextConfidence, TextLineKind,
    TextStrategy, write_xmind_bytes,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in entries {
        writer.start_file(*name, options).expect("start ZIP entry");
        writer.write_all(data).expect("write ZIP entry");
    }
    writer.finish().expect("finish ZIP").into_inner()
}

fn build_docx(document_xml: &str) -> Vec<u8> {
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}">
  <w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:styleId="InheritedHeading"><w:name w:val="Inherited Heading"/><w:basedOn w:val="Heading2"/></w:style>
  <w:style w:type="paragraph" w:styleId="MysteryStyle"><w:name w:val="Mystery Style"/><w:basedOn w:val="Normal"/></w:style>
</w:styles>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{W_NS}">
  <w:abstractNum w:abstractNumId="10"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
  <w:abstractNum w:abstractNumId="20"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="10"/></w:num>
  <w:num w:numId="2"><w:abstractNumId w:val="20"/></w:num>
</w:numbering>"#
    );
    let relations = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/index" TargetMode="External"/>
  <Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="{W_NS}">
  <w:footnote w:id="1" w:type="separator"><w:p><w:r><w:t>separator</w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="2"><w:p><w:r><w:t>Footnote detail with https://example.com/note.</w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="5"><w:p><w:r><w:t>Table footnote detail.</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:endnotes xmlns:w="{W_NS}">
  <w:endnote w:id="3"><w:p><w:r><w:t>Endnote detail.</w:t></w:r></w:p></w:endnote>
</w:endnotes>"#
    );
    let comments = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{W_NS}">
  <w:comment w:id="4" w:author="Reviewer" w:date="2026-11-27T00:00:00Z"><w:p><w:r><w:t>Please verify this.</w:t></w:r></w:p></w:comment>
</w:comments>"#
    );
    build_zip(&[
        ("word/document.xml", document_xml.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/_rels/document.xml.rels", relations.as_bytes()),
        ("word/footnotes.xml", footnotes.as_bytes()),
        ("word/endnotes.xml", endnotes.as_bytes()),
        ("word/comments.xml", comments.as_bytes()),
        ("word/media/image1.png", b"fake-png-payload"),
    ])
}

fn rich_document_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}" xmlns:a="{A_NS}" xmlns:wp="{WP_NS}" xmlns:pic="{PIC_NS}">
<w:body>
  <w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Architecture Map</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>API Performance</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Indexing</w:t></w:r></w:p>
  <w:p><w:r><w:t>响应时间过长。Main bottleneck is indexing; Release v1.2.3 costs 3.2s. See https://example.com/a.b?x=1.2</w:t></w:r></w:p>
  <w:p>
    <w:hyperlink r:id="rId5"><w:r><w:t>Docs</w:t></w:r></w:hyperlink>
    <w:r><w:drawing><wp:inline><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId6"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>
    <w:r><w:footnoteReference w:id="2"/></w:r>
    <w:r><w:endnoteReference w:id="3"/></w:r>
    <w:r><w:commentReference w:id="4"/></w:r>
    <w:ins w:id="7"><w:r><w:t> Added text.</w:t></w:r></w:ins>
    <w:del w:id="8"><w:r><w:delText> Deleted text.</w:delText></w:r></w:del>
  </w:p>
  <w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Install. Configure.</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>Fast</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>Native</w:t></w:r></w:p>
  <w:tbl>
    <w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>Query</w:t></w:r><w:r><w:footnoteReference w:id="5"/></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Latency</w:t></w:r></w:p></w:tc></w:tr>
    <w:tr><w:tc><w:p><w:r><w:t>List</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>12.5ms</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
  <w:p><w:pPr><w:pStyle w:val="InheritedHeading"/></w:pPr><w:r><w:t>Inherited Section</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="MysteryStyle"/></w:pPr><w:r><w:t>Custom-style paragraph.</w:t></w:r></w:p>
  <w:sectPr/>
</w:body>
</w:document>"#
    )
}

fn rich_docx() -> Vec<u8> {
    build_docx(&rich_document_xml())
}

fn extract_docx(bytes: &[u8], options: &ConversionOptions) -> OutlineDocument {
    let adapter = DocxAdapter::default();
    adapter
        .extract(
            SourceInput {
                path: Some(Path::new("architecture.docx")),
                bytes,
                format: &SourceFormat::Docx,
            },
            options,
        )
        .expect("extract DOCX")
}

fn extract_text(bytes: &[u8], options: &ConversionOptions) -> OutlineDocument {
    TextAdapter::default()
        .extract(
            SourceInput {
                path: Some(Path::new("meeting.txt")),
                bytes,
                format: &SourceFormat::Text,
            },
            options,
        )
        .expect("extract TXT")
}

fn walk<'a>(nodes: &'a [OutlineNode], output: &mut Vec<&'a OutlineNode>) {
    for node in nodes {
        output.push(node);
        walk(&node.children, output);
    }
}

fn all_nodes(document: &OutlineDocument) -> Vec<&OutlineNode> {
    let mut nodes = Vec::new();
    walk(&document.children, &mut nodes);
    nodes
}

fn sentence_nodes(document: &OutlineDocument) -> Vec<&OutlineNode> {
    all_nodes(document)
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind,
                OutlineNodeKind::Content {
                    kind: ContentKind::Sentence,
                    ..
                }
            )
        })
        .collect()
}

#[test]
fn docx_preserves_structural_objects_and_reports_degradations() {
    let bytes = rich_docx();
    let options = ConversionOptions {
        min_fragment_chars: 1,
        ..ConversionOptions::default()
    };
    let document = extract_docx(&bytes, &options);
    assert_eq!(document.title, "Architecture Map");
    assert_eq!(document.source.format, SourceFormat::Docx);
    assert_eq!(document.source.source_sha256.len(), 64);
    assert_eq!(document.table_count(), 1);
    assert_eq!(document.link_count(), 1);
    assert_eq!(document.attachment_count(), 1);

    let nodes = all_nodes(&document);
    let headings = nodes
        .iter()
        .filter(|node| node.title == "API Performance")
        .collect::<Vec<_>>();
    assert_eq!(headings.len(), 1);
    assert!(nodes.iter().any(|node| node.title == "Indexing"));
    assert!(nodes.iter().any(|node| node.title == "Inherited Section"));
    let paragraphs = sentence_nodes(&document);
    assert!(paragraphs.len() >= 4, "{paragraphs:#?}");
    assert!(
        paragraphs
            .iter()
            .any(|node| node.title.contains("响应时间"))
    );
    assert!(paragraphs.iter().any(|node| node.title.contains("v1.2.3")));
    assert!(
        paragraphs
            .iter()
            .any(|node| node.title.contains("https://example.com/a.b?x=1.2"))
    );
    assert!(nodes.iter().any(|node| node.title == "Install."));
    assert!(nodes.iter().any(|node| node.title == "Configure."));
    assert!(nodes.iter().any(|node| node.title == "Fast"));
    assert!(nodes.iter().any(|node| node.title == "Native"));

    let attachment = nodes
        .iter()
        .flat_map(|node| &node.attachments)
        .next()
        .expect("image attachment");
    assert_eq!(attachment.name, "image1.png");
    assert_eq!(
        attachment.data.as_deref(),
        Some(b"fake-png-payload".as_slice())
    );
    assert!(nodes.iter().any(|node| {
        node.links
            .iter()
            .any(|link| link.target == "https://example.com/index")
    }));
    assert!(nodes.iter().any(|node| {
        node.tables
            .iter()
            .any(|table| table.headers == ["Query", "Latency"])
    }));
    assert!(nodes.iter().any(|node| {
        node.notes
            .iter()
            .any(|note| note.text.contains("Footnote detail"))
    }));
    assert!(nodes.iter().any(|node| {
        node.notes
            .iter()
            .any(|note| note.text.contains("Table footnote detail"))
    }));
    assert!(nodes.iter().any(|node| {
        node.notes.iter().any(|note| {
            note.text
                .contains("响应时间过长。Main bottleneck is indexing; Release v1.2.3")
        })
    }));
    assert!(nodes.iter().any(|node| {
        node.notes
            .iter()
            .any(|note| note.text.contains("Endnote detail"))
    }));
    assert!(nodes.iter().any(|node| {
        node.notes
            .iter()
            .any(|note| note.text.contains("Please verify this"))
    }));
    assert!(!nodes.iter().any(|node| node.title.contains("Deleted text")));
    assert!(nodes.iter().any(|node| node.title.contains("Added text")));

    assert!(document.warnings.iter().any(|warning| {
        warning.code == ConversionWarningCode::UnsupportedElement
            && warning.message.contains("tracked-change")
    }));
    assert!(document.warnings.iter().any(|warning| {
        warning.code == ConversionWarningCode::UnsupportedElement
            && warning.message.contains("MysteryStyle")
    }));
    assert!(document.warnings.iter().any(|warning| {
        warning.code == ConversionWarningCode::UnsupportedElement
            && warning.message.contains("comment")
    }));
}

#[test]
fn docx_heuristic_headings_require_the_explicit_switch() {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="{W_NS}"><w:body>
  <w:p><w:pPr><w:pStyle w:val="BoldTitle"/><w:rPr><w:b/><w:sz w:val="36"/></w:rPr></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="36"/></w:rPr><w:t>Inferred Heading</w:t></w:r></w:p>
  <w:p><w:r><w:t>Body sentence. Second sentence.</w:t></w:r></w:p>
</w:body></w:document>"#
    );
    let bytes = build_docx(&document_xml);
    let options = ConversionOptions {
        min_fragment_chars: 1,
        ..ConversionOptions::default()
    };
    let default_document = extract_docx(&bytes, &options);
    let default_heading = all_nodes(&default_document)
        .into_iter()
        .find(|node| node.title == "Inferred Heading")
        .expect("text is preserved as body content");
    assert!(!matches!(
        default_heading.kind,
        OutlineNodeKind::Structure { .. }
    ));

    let adapter = DocxAdapter::new(DocxAdapterOptions {
        heading_heuristics: true,
    });
    let inferred = adapter
        .extract(
            SourceInput {
                path: Some(Path::new("heuristic.docx")),
                bytes: &bytes,
                format: &SourceFormat::Docx,
            },
            &options,
        )
        .expect("extract heuristic DOCX");
    let heading = all_nodes(&inferred)
        .into_iter()
        .find(|node| node.title == "Inferred Heading")
        .expect("heuristic heading");
    assert!(matches!(
        heading.kind,
        OutlineNodeKind::Structure {
            kind: kite_core::mindmap_import::StructureKind::Heading { .. }
        }
    ));
    assert!(inferred.warnings.iter().any(|warning| {
        warning.code == ConversionWarningCode::InvalidStructure
            && warning.message.contains("bold/large")
    }));
}

#[test]
fn docx_supports_direct_outline_levels() {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="{W_NS}"><w:body>
  <w:p><w:pPr><w:outlineLvl w:val="1"/></w:pPr><w:r><w:t>Outline Level Two</w:t></w:r></w:p>
  <w:p><w:r><w:t>Body.</w:t></w:r></w:p>
</w:body></w:document>"#
    );
    let bytes = build_docx(&document_xml);
    let document = extract_docx(&bytes, &ConversionOptions::default());
    let heading = all_nodes(&document)
        .into_iter()
        .find(|node| node.title == "Outline Level Two")
        .expect("outline heading");
    assert!(matches!(
        heading.kind,
        OutlineNodeKind::Structure {
            kind: kite_core::mindmap_import::StructureKind::Heading { level: 2 }
        }
    ));
}

#[test]
fn malformed_docx_inputs_fail_explicitly() {
    let adapter = DocxAdapter::default();
    let options = ConversionOptions::default();
    let malformed = adapter.extract(
        SourceInput {
            path: Some(Path::new("broken.docx")),
            bytes: b"not a zip",
            format: &SourceFormat::Docx,
        },
        &options,
    );
    assert!(malformed.is_err());

    let missing_document = build_zip(&[("word/styles.xml", b"<w:styles/>")]);
    let missing = adapter.extract(
        SourceInput {
            path: Some(Path::new("missing.docx")),
            bytes: &missing_document,
            format: &SourceFormat::Docx,
        },
        &options,
    );
    assert!(
        matches!(missing, Err(kite_core::mindmap_import::ImportError::InvalidDocument(message)) if message.contains("word/document.xml"))
    );

    let malformed_xml = build_zip(&[("word/document.xml", b"<w:document>")]);
    let invalid = adapter.extract(
        SourceInput {
            path: Some(Path::new("invalid.docx")),
            bytes: &malformed_xml,
            format: &SourceFormat::Docx,
        },
        &options,
    );
    assert!(invalid.is_err());
}

#[test]
fn txt_detects_chinese_english_structure_and_protects_sentence_syntax() {
    let input = r#"第一章 项目概览

1. 目标
  响应时间过长。Main bottleneck is indexing; Release v1.2.3 costs 3.2s. See https://example.com/a.b?x=1.2
  - Rust Core
    - macOS
    - Windows

Chapter 2 Architecture

背景
下一句仍然是正文，没有标题上下文。
"#;
    let adapter = TextAdapter::default();
    let options = ConversionOptions {
        min_fragment_chars: 1,
        ..ConversionOptions::default()
    };
    let source = SourceInput {
        path: Some(Path::new("plan.txt")),
        bytes: input.as_bytes(),
        format: &SourceFormat::Text,
    };
    let analysis = adapter.analyze(source, &options).expect("analyze TXT");
    assert_eq!(analysis.strategy, TextStrategy::SmartLayering);
    assert!(analysis.heading_count >= 2);
    assert!(analysis.list_item_count >= 3);
    assert!(analysis.max_depth <= 4);
    assert!(analysis.structure_confidence == TextConfidence::High);
    assert!(
        analysis
            .lines
            .iter()
            .any(|line| matches!(line.kind, TextLineKind::Heading { .. }))
    );

    let document = extract_text(input.as_bytes(), &options);
    assert_eq!(document.title, "meeting");
    let nodes = all_nodes(&document);
    assert!(nodes.iter().any(|node| node.title == "项目概览"));
    assert!(nodes.iter().any(|node| node.title == "目标"));
    assert!(nodes.iter().any(|node| node.title == "Architecture"));
    assert!(nodes.iter().any(|node| node.title == "Rust Core"));
    assert!(nodes.iter().any(|node| node.title == "macOS"));
    assert!(nodes.iter().any(|node| node.title == "Windows"));
    assert!(nodes.iter().any(|node| {
        node.notes
            .iter()
            .any(|note| note.text.contains("https://example.com/a.b?x=1.2"))
    }));
    assert!(
        sentence_nodes(&document)
            .iter()
            .any(|node| node.title.contains("v1.2.3"))
    );
    assert!(
        document
            .warnings
            .iter()
            .any(|warning| warning.code == ConversionWarningCode::InvalidStructure)
    );
}

#[test]
fn txt_detects_chinese_and_parenthesized_numbering_without_losing_titles() {
    let input = "一、需求
  正文。

(1) 安装依赖
(2) 运行测试
";
    let document = extract_text(input.as_bytes(), &ConversionOptions::default());
    let nodes = all_nodes(&document);
    assert!(nodes.iter().any(|node| node.title == "需求"));
    assert!(nodes.iter().any(|node| node.title == "安装依赖"));
    assert!(nodes.iter().any(|node| node.title == "运行测试"));
}

#[test]
fn txt_presets_produce_strict_smart_and_sentence_split_shapes() {
    let input = "背景\n\n正文第一句内容足够长。正文第二句内容足够长；正文第三句内容足够长。\n";
    let strict = extract_text(input.as_bytes(), &ConversionOptions::strict_outline());
    assert!(sentence_nodes(&strict).is_empty());
    assert!(all_nodes(&strict).iter().any(|node| matches!(
        node.kind,
        OutlineNodeKind::Content {
            kind: ContentKind::Paragraph,
            ..
        }
    )));

    let smart = extract_text(input.as_bytes(), &ConversionOptions::smart_layering());
    assert!(all_nodes(&smart).iter().any(|node| node.title == "背景"));

    let split = extract_text(input.as_bytes(), &ConversionOptions::sentence_split());
    let split_sentences = sentence_nodes(&split);
    assert_eq!(split_sentences.len(), 4);
    assert!(split_sentences.iter().any(|node| node.title == "背景"));
    assert_eq!(split.source.original_format["strategy"], "sentence_split");
    assert!(matches!(
        split.report(&ConversionOptions::sentence_split()).preset,
        ConversionPreset::SentenceSplit
    ));
}

#[test]
fn txt_long_paragraph_uses_grouped_sentences_instead_of_a_flat_explosion() {
    let sentences = (1..=12)
        .map(|index| {
            if index == 6 {
                format!("Detailed sentence number {index} keeps version v1.2.3 and 3.2 seconds.")
            } else {
                format!("Detailed sentence number {index} remains readable and actionable.")
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let input = format!("Topic\n\n{sentences}\n");
    let document = extract_text(
        input.as_bytes(),
        &ConversionOptions {
            min_fragment_chars: 1,
            ..ConversionOptions::default()
        },
    );
    let groups = all_nodes(&document)
        .into_iter()
        .filter(|node| node.title.starts_with("Paragraph "))
        .collect::<Vec<_>>();
    assert_eq!(groups.len(), 3);
    assert!(groups.iter().all(|group| !group.children.is_empty()));
    assert!(document.stats.split_fragments >= 12);
    assert!(
        all_nodes(&document)
            .iter()
            .any(|node| node.title.contains("v1.2.3"))
    );
}

#[test]
fn txt_mixed_indentation_and_ambiguous_short_lines_are_marked_low_confidence() {
    let input = "背景\n下一句正文。\n\n- one\n\t- two\n  - three\n";
    let adapter = TextAdapter::new(TextAdapterOptions::default());
    let options = ConversionOptions::default();
    let analysis = adapter
        .analyze(
            SourceInput {
                path: Some(Path::new("mixed.txt")),
                bytes: input.as_bytes(),
                format: &SourceFormat::Text,
            },
            &options,
        )
        .expect("analyze mixed TXT");
    assert!(analysis.mixed_indentation);
    assert!(analysis.ambiguous_short_line_count >= 1);
    assert!(
        analysis
            .lines
            .iter()
            .any(|line| line.confidence == TextConfidence::Low)
    );
    let document = extract_text(input.as_bytes(), &options);
    assert_eq!(
        document.source.original_format["mixed_indentation"],
        serde_json::Value::Bool(true)
    );
}

#[test]
fn txt_rejects_invalid_utf8_and_handles_utf16_bom() {
    let invalid = TextAdapter::default().extract(
        SourceInput {
            path: Some(Path::new("bad.txt")),
            bytes: &[0xFF, 0xFE, 0x00],
            format: &SourceFormat::Text,
        },
        &ConversionOptions::default(),
    );
    assert!(invalid.is_err());

    let text = "第一章 标题\r\n\r\n正文第一句。正文第二句。";
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let document = extract_text(&utf16, &ConversionOptions::default());
    assert_eq!(document.title, "meeting");
    assert_eq!(document.source.original_format["encoding"], "utf-16le");
    assert!(all_nodes(&document).iter().any(|node| node.title == "标题"));
}

#[test]
fn both_adapters_produce_packages_accepted_by_the_production_reader() {
    for (document, options) in [
        (
            extract_docx(&rich_docx(), &ConversionOptions::default()),
            ConversionOptions::default(),
        ),
        (
            extract_text(
                "第一章 标题\n\n第一句。第二句；第三句。".as_bytes(),
                &ConversionOptions::default(),
            ),
            ConversionOptions::default(),
        ),
    ] {
        let result = write_xmind_bytes(&document, &options).expect("write XMind");
        XmindPackage::from_bytes(result.bytes).expect("production reader accepts XMind");
    }
}
