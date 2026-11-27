use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use kite_core::mindmap::{CONTENT_JSON_ENTRY, XmindPackage};
use kite_core::mindmap_import::{
    ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode, LinkKind,
    OutlineAttachment, OutlineDocument, OutlineLink, OutlineNode, OutlineNodeKind, OutlineTable,
    SentenceSplitOptions, SourceFormat, SourceLocation, SourceProvenance, StructureKind,
    add_paragraph_as_content_nodes, segment_sentences, write_xmind_bytes,
};
use serde_json::Value;
use zip::ZipArchive;

fn source_provenance() -> SourceProvenance {
    let mut source = SourceProvenance::new(SourceFormat::Markdown, "fixture-adapter", "1");
    source.source_sha256 = "a".repeat(64);
    source.source_path = Some("docs/performance.md".to_string());
    source
}

fn sample_document() -> OutlineDocument {
    let mut document = OutlineDocument::new("性能治理", source_provenance());
    let mut heading = OutlineNode::structure("接口性能", StructureKind::Heading { level: 1 })
        .with_source(SourceLocation {
            path: vec!["接口性能".to_string()],
            line: Some(10),
            column: None,
            byte_offset: Some(120),
        });
    let paragraph = "接口响应时间过长。主要瓶颈在索引缺失；其次是重复查询。";
    let mut options = ConversionOptions {
        min_fragment_chars: 1,
        ..ConversionOptions::default()
    };
    options.max_title_chars = 6;
    let heading_source = heading.source.clone();
    let expansion =
        add_paragraph_as_content_nodes(&mut heading, paragraph, heading_source, &options)
            .expect("split paragraph");

    assert_eq!(expansion.nodes_added, 3);
    assert!(expansion.original_preserved);
    assert!(
        expansion
            .warnings
            .iter()
            .any(|warning| warning.code == ConversionWarningCode::TitleTruncated)
    );
    assert!(heading.notes.iter().any(|note| note.text == paragraph));
    assert!(heading.children.iter().all(OutlineNode::is_content));

    let indexed = heading
        .children
        .iter_mut()
        .find(|node| node.title.contains("瓶颈"))
        .expect("index sentence");
    indexed.links.push(OutlineLink {
        label: Some("索引说明".to_string()),
        target: "https://example.com/index".to_string(),
        kind: LinkKind::Hyperlink,
        source: indexed.source.clone(),
    });
    indexed.tables.push(OutlineTable {
        caption: Some("查询耗时".to_string()),
        headers: vec!["查询".to_string(), "耗时".to_string()],
        rows: vec![vec!["列表".to_string(), "180ms".to_string()]],
        source: indexed.source.clone(),
    });
    indexed.attachments.push(OutlineAttachment {
        name: "trace.txt".to_string(),
        media_type: Some("text/plain".to_string()),
        uri: Some("attachments/trace.txt".to_string()),
        data: None,
        source: indexed.source.clone(),
    });

    document.children.push(heading);
    document.warnings.push(ConversionWarning::warning(
        ConversionWarningCode::UnsupportedElement,
        "Source contained one unsupported comment",
        Some(SourceLocation {
            path: vec!["接口性能".to_string()],
            line: Some(16),
            column: None,
            byte_offset: None,
        }),
    ));
    document
}

#[test]
fn structure_and_content_nodes_remain_distinct() {
    let document = sample_document();
    let heading = &document.children[0];

    assert!(matches!(
        heading.kind,
        OutlineNodeKind::Structure {
            kind: StructureKind::Heading { level: 1 }
        }
    ));
    assert_eq!(document.structure_node_count(), 1);
    assert_eq!(document.content_node_count(), 3);
    for node in &heading.children {
        assert!(matches!(
            node.kind,
            OutlineNodeKind::Content {
                kind: ContentKind::Sentence,
                split_from_paragraph: true
            }
        ));
    }
}

#[test]
fn sentence_ranges_protect_syntax_and_preserve_source_offsets() {
    let input = "版本 v1.2.3：https://example.com/a。括号内不拆 (keep this. together.)；结束。";
    let fragments = segment_sentences(
        input,
        &SentenceSplitOptions {
            min_fragment_chars: 1,
            merge_short_fragments: false,
        },
    );

    assert_eq!(fragments.len(), 3);
    for fragment in &fragments {
        assert_eq!(&input[fragment.start..fragment.end], fragment.text);
    }
    assert!(fragments[0].text.contains("v1.2.3"));
    assert!(fragments[0].text.contains("https://example.com/a"));
    assert!(fragments[1].text.contains("keep this. together."));
}

#[test]
fn writes_valid_deterministic_xmind_with_notes_and_report() {
    let document = sample_document();
    let options = ConversionOptions {
        min_fragment_chars: 1,
        max_title_chars: 120,
        include_source_path: true,
        ..ConversionOptions::default()
    };

    let first = write_xmind_bytes(&document, &options).expect("write first package");
    let second = write_xmind_bytes(&document, &options).expect("write second package");
    assert_eq!(
        first.bytes, second.bytes,
        "writer output must be deterministic"
    );
    assert_eq!(first.report.node_count, 4);
    assert_eq!(first.report.structure_node_count, 1);
    assert_eq!(first.report.content_node_count, 3);
    assert_eq!(first.report.table_count, 1);
    assert_eq!(first.report.attachment_count, 1);
    assert!(
        first
            .report
            .warnings
            .iter()
            .any(|warning| warning.code == ConversionWarningCode::UnsupportedElement)
    );
    assert!(
        first
            .report
            .warnings
            .iter()
            .any(|warning| warning.code == ConversionWarningCode::TableFlattened)
    );
    assert!(
        first
            .report
            .warnings
            .iter()
            .any(|warning| warning.code == ConversionWarningCode::AttachmentReferenceOnly)
    );

    let package =
        XmindPackage::from_bytes(first.bytes.clone()).expect("production reader accepts writer");
    let entries = package
        .entries()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entries,
        BTreeSet::from(["content.json", "manifest.json", "metadata.json",])
    );

    let content = package.content();
    let root = &content[0]["rootTopic"];
    assert_eq!(root["title"], Value::String("性能治理".to_string()));
    assert_eq!(
        root["children"]["attached"][0]["title"],
        Value::String("接口性能".to_string())
    );
    let root_notes = root["notes"]["plain"]["content"]
        .as_str()
        .expect("root provenance notes");
    assert!(root_notes.contains("Source SHA-256:"));
    assert!(root_notes.contains("docs/performance.md"));

    let heading = &root["children"]["attached"][0];
    let heading_notes = heading["notes"]["plain"]["content"]
        .as_str()
        .expect("paragraph note");
    assert!(heading_notes.contains("接口响应时间过长。主要瓶颈在索引缺失；其次是重复查询。"));

    let indexed = find_topic(heading, |title| title.contains("瓶颈")).expect("index content topic");
    assert_eq!(
        indexed["href"],
        Value::String("https://example.com/index".to_string())
    );
    let indexed_notes = indexed["notes"]["plain"]["content"]
        .as_str()
        .expect("table and attachment notes");
    assert!(indexed_notes.contains("| 查询 | 耗时 |"));
    assert!(indexed_notes.contains("Attachment: trace.txt"));

    let mut archive = ZipArchive::new(Cursor::new(first.bytes)).expect("open writer ZIP");
    let mut content_json = Vec::new();
    archive
        .by_name(CONTENT_JSON_ENTRY)
        .expect("content entry")
        .read_to_end(&mut content_json)
        .expect("read content entry");
    assert!(!content_json.is_empty());
}

fn find_topic(topic: &Value, predicate: impl Fn(&str) -> bool + Copy) -> Option<&Value> {
    if topic
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(predicate)
    {
        return Some(topic);
    }
    let children = topic
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array);
    if let Some(children) = children {
        for child in children {
            if let Some(found) = find_topic(child, predicate) {
                return Some(found);
            }
        }
    }
    None
}
