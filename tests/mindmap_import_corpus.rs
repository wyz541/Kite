use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kite_core::mindmap::{CONTENT_JSON_ENTRY, XmindPackage};
use kite_core::mindmap_import::{
    ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode, DocxAdapter,
    HtmlAdapter, ImportError, MarkdownAdapter, OpmlAdapter, OutlineAdapter, OutlineDocument,
    OutlineNode, OutlineNodeKind, SourceFormat, SourceInput, TaskState, TextAdapter,
    write_xmind_bytes,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const CORPUS_DIR: &str = "tests/fixtures/mindmap/import";
const MANIFEST_PATH: &str = "tests/fixtures/mindmap/import/manifest.json";
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusManifest {
    schema_version: u32,
    corpus_sha256: String,
    fixtures: Vec<FixtureEntry>,
    performance_sanity: PerformanceSanity,
}

#[derive(Debug, Deserialize)]
struct FixtureEntry {
    path: String,
    format: String,
    role: String,
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerformanceSanity {
    synthetic_topics: usize,
    expected_outline_nodes: usize,
    expected_package_topics: usize,
    max_package_bytes: usize,
    max_elapsed_ms: u64,
}

fn corpus_options() -> ConversionOptions {
    ConversionOptions {
        min_fragment_chars: 1,
        merge_short_fragments: false,
        ..ConversionOptions::default()
    }
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS_DIR)
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_PATH)
}

fn load_manifest() -> CorpusManifest {
    serde_json::from_slice(&fs::read(manifest_path()).expect("read corpus manifest"))
        .expect("parse corpus manifest")
}

fn fixture_entry<'a>(manifest: &'a CorpusManifest, path: &str) -> &'a FixtureEntry {
    manifest
        .fixtures
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("missing fixture {path:?}"))
}

fn role_entries<'a>(
    manifest: &'a CorpusManifest,
    role: &str,
    format: &str,
) -> Vec<&'a FixtureEntry> {
    manifest
        .fixtures
        .iter()
        .filter(|entry| entry.role == role && entry.format == format)
        .collect()
}

fn source_format(value: &str) -> SourceFormat {
    match value {
        "markdown" => SourceFormat::Markdown,
        "docx" => SourceFormat::Docx,
        "text" => SourceFormat::Text,
        "html" => SourceFormat::Html,
        "opml" => SourceFormat::Opml,
        other => panic!("unsupported corpus format {other:?}"),
    }
}

fn extract_fixture(entry: &FixtureEntry) -> Result<OutlineDocument, ImportError> {
    let path = corpus_root().join(&entry.path);
    let bytes = fs::read(&path).expect("read corpus fixture");
    let format = source_format(&entry.format);
    let source = SourceInput {
        path: Some(&path),
        bytes: &bytes,
        format: &format,
    };
    let options = corpus_options();
    match &format {
        SourceFormat::Markdown => MarkdownAdapter.extract(source, &options),
        SourceFormat::Docx => DocxAdapter::default().extract(source, &options),
        SourceFormat::Text => TextAdapter::default().extract(source, &options),
        SourceFormat::Html => HtmlAdapter.extract(source, &options),
        SourceFormat::Opml => OpmlAdapter.extract(source, &options),
        _ => unreachable!("corpus source format"),
    }
}

fn document(manifest: &CorpusManifest, path: &str) -> OutlineDocument {
    extract_fixture(fixture_entry(manifest, path)).unwrap_or_else(|error| {
        panic!("fixture {path:?} failed extraction: {error}");
    })
}

fn child<'a>(node: &'a OutlineNode, title: &str) -> &'a OutlineNode {
    node.children
        .iter()
        .find(|child| child.title == title)
        .unwrap_or_else(|| {
            panic!(
                "missing child {title:?}; children: {:?}",
                node.children
                    .iter()
                    .map(|child| &child.title)
                    .collect::<Vec<_>>()
            )
        })
}

fn top<'a>(document: &'a OutlineDocument, title: &str) -> &'a OutlineNode {
    document
        .children
        .iter()
        .find(|node| node.title == title)
        .unwrap_or_else(|| {
            panic!(
                "missing top-level node {title:?}; nodes: {:?}",
                document
                    .children
                    .iter()
                    .map(|node| &node.title)
                    .collect::<Vec<_>>()
            )
        })
}

fn all_nodes(document: &OutlineDocument) -> Vec<&OutlineNode> {
    fn visit<'a>(node: &'a OutlineNode, output: &mut Vec<&'a OutlineNode>) {
        output.push(node);
        for child in &node.children {
            visit(child, output);
        }
    }

    let mut output = Vec::new();
    for node in &document.children {
        visit(node, &mut output);
    }
    output
}

fn sentence_titles(node: &OutlineNode) -> Vec<&str> {
    node.children
        .iter()
        .filter(|child| {
            matches!(
                child.kind,
                OutlineNodeKind::Content {
                    kind: ContentKind::Sentence,
                    split_from_paragraph: true
                }
            )
        })
        .map(|child| child.title.as_str())
        .collect()
}

fn has_warning(document: &OutlineDocument, code: ConversionWarningCode, needle: &str) -> bool {
    document
        .warnings
        .iter()
        .any(|warning| warning.code == code && warning.message.contains(needle))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn fixture_manifest_is_complete_hashed_and_role_balanced() {
    let manifest = load_manifest();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.fixtures.len(), 15);

    let mut aggregate_input = String::new();
    for entry in &manifest.fixtures {
        let bytes = fs::read(corpus_root().join(&entry.path))
            .unwrap_or_else(|error| panic!("read {}: {error}", entry.path));
        assert_eq!(bytes.len(), entry.bytes, "size drift for {}", entry.path);
        assert_eq!(
            sha256_hex(&bytes),
            entry.sha256,
            "hash drift for {}",
            entry.path
        );
        aggregate_input.push_str(&format!("{}\0{}\n", entry.path, entry.sha256));
    }
    assert_eq!(
        sha256_hex(aggregate_input.as_bytes()),
        manifest.corpus_sha256,
        "corpus aggregate hash drift"
    );

    for format in ["markdown", "docx", "text", "html", "opml"] {
        for role in ["normal", "edge", "malformed"] {
            assert_eq!(
                role_entries(&manifest, role, format).len(),
                1,
                "expected exactly one {role} {format} fixture"
            );
        }
    }
}

#[test]
fn normal_corpus_has_expected_hierarchy_notes_warnings_and_round_trips() {
    let manifest = load_manifest();
    let options = corpus_options();

    let markdown = document(&manifest, "markdown-normal.md");
    assert_eq!(markdown.title, "Import Corpus");
    let overview = top(&markdown, "Overview");
    assert_eq!(
        sentence_titles(overview),
        vec![
            "Overview sentence one is long enough.",
            "Overview sentence two is long enough;",
            "Overview sentence three is long enough.",
        ]
    );
    assert!(overview.notes.iter().any(|note| {
        note.label.as_deref() == Some("Original paragraph")
            && note.text
                == "Overview sentence one is long enough. Overview sentence two is long enough; Overview sentence three is long enough."
    }));
    let planning = top(&markdown, "Planning");
    assert_eq!(
        child(planning, "Build adapters").task,
        Some(TaskState::Todo)
    );
    assert_eq!(
        child(child(planning, "Build adapters"), "Markdown").task,
        Some(TaskState::Done)
    );
    let details = child(planning, "Details");
    assert_eq!(details.tables[0].headers, vec!["Field", "Value"]);
    assert!(
        details
            .links
            .iter()
            .any(|link| { link.target == "https://example.com/design" })
    );
    assert!(
        details
            .children
            .iter()
            .any(|node| node.title == "Code block · rust")
    );
    assert!(
        details
            .children
            .iter()
            .any(|node| node.title == "Image: diagram")
    );
    assert!(has_warning(
        &markdown,
        ConversionWarningCode::UnsupportedElement,
        "Image is represented as a link"
    ));

    let docx = document(&manifest, "docx-normal.docx");
    assert_eq!(docx.title, "DOCX Import Corpus");
    let architecture = top(&docx, "Architecture");
    assert_eq!(
        sentence_titles(architecture),
        vec![
            "Storage sentence one is long enough.",
            "Storage sentence two is long enough;",
            "Storage sentence three is long enough.",
        ]
    );
    assert!(architecture.notes.iter().any(|note| {
        note.text
            == "Storage sentence one is long enough. Storage sentence two is long enough; Storage sentence three is long enough."
    }));
    let indexes = child(architecture, "Indexes");
    assert_eq!(
        sentence_titles(indexes),
        vec!["Index sentence one is long enough."]
    );

    let html = document(&manifest, "html-normal.html");
    assert_eq!(html.title, "HTML Import Corpus");
    let html_overview = top(&html, "Overview");
    assert_eq!(sentence_titles(html_overview).len(), 3);
    let html_details = child(html_overview, "Details");
    assert_eq!(html_details.tables[0].headers, vec!["Field", "Value"]);
    assert_eq!(
        html_details.tables[0].rows,
        vec![vec!["Nodes".to_string(), "3000".to_string()]]
    );
    assert!(
        html_details
            .children
            .iter()
            .any(|node| node.title == "Image: diagram")
    );
    assert!(has_warning(
        &html,
        ConversionWarningCode::UnsupportedElement,
        "Image is represented as a reference"
    ));

    let opml = document(&manifest, "opml-normal.opml");
    assert_eq!(opml.title, "OPML Import Corpus");
    let platforms = top(&opml, "Platforms");
    assert_eq!(platforms.task, Some(TaskState::Todo));
    assert!(
        platforms
            .notes
            .iter()
            .any(|note| note.text == "Shared Rust Core.")
    );
    assert_eq!(child(platforms, "macOS").task, Some(TaskState::Done));
    assert_eq!(child(platforms, "Windows").task, Some(TaskState::Todo));
    assert!(
        child(platforms, "Rust Core")
            .links
            .iter()
            .any(|link| link.target == "https://example.com/rust")
    );

    let text = document(&manifest, "text-normal.txt");
    assert_eq!(text.title, "text-normal");
    let performance = top(&text, "性能治理");
    assert_eq!(
        sentence_titles(performance),
        vec![
            "接口响应时间过长。",
            "主要瓶颈在索引缺失；",
            "其次是重复查询。",
            "版本 v1.2.3 保持不变。",
        ]
    );
    assert!(performance.notes.iter().any(|note| {
        note.text.contains(
            "接口响应时间过长。主要瓶颈在索引缺失；其次是重复查询。版本 v1.2.3 保持不变。",
        )
    }));
    assert_eq!(
        all_nodes(&text)
            .into_iter()
            .filter(|node| node.title == "Markdown")
            .count(),
        1
    );

    let cases = [
        (
            "markdown-normal.md",
            6,
            vec![ConversionWarningCode::UnsupportedElement],
            vec![
                ConversionWarningCode::UnsupportedElement,
                ConversionWarningCode::TableFlattened,
                ConversionWarningCode::AttachmentReferenceOnly,
            ],
        ),
        ("docx-normal.docx", 4, vec![], vec![]),
        (
            "html-normal.html",
            5,
            vec![
                ConversionWarningCode::UnsupportedElement,
                ConversionWarningCode::UnsupportedElement,
            ],
            vec![
                ConversionWarningCode::UnsupportedElement,
                ConversionWarningCode::UnsupportedElement,
                ConversionWarningCode::TableFlattened,
                ConversionWarningCode::AttachmentReferenceOnly,
            ],
        ),
        ("opml-normal.opml", 0, vec![], vec![]),
        ("text-normal.txt", 4, vec![], vec![]),
    ];
    for (path, expected_split_fragments, adapter_warnings, writer_warnings) in cases {
        let document = document(&manifest, path);
        assert_eq!(
            document.stats.split_fragments, expected_split_fragments,
            "split fragment count drift for {path}"
        );
        assert_eq!(
            document.stats.merged_fragments, 0,
            "merged drift for {path}"
        );
        assert_eq!(
            document.stats.truncated_titles, 0,
            "truncated title drift for {path}"
        );
        assert_eq!(
            document.stats.skipped_items, 0,
            "skipped item drift for {path}"
        );
        assert_eq!(
            warning_codes(&document.warnings),
            adapter_warnings,
            "adapter warning code/order drift for {path}"
        );
        assert_xmind_round_trip(path, &document, &options, &writer_warnings);
    }
}

#[test]
fn warning_and_stats_contracts_cover_skip_merge_and_truncation_exactly() {
    let manifest = load_manifest();

    let docx_entry = fixture_entry(&manifest, "docx-normal.docx");
    let docx_path = corpus_root().join(&docx_entry.path);
    let docx_bytes = fs::read(&docx_path).expect("read DOCX fixture");
    let docx_options = ConversionOptions {
        split_content: false,
        merge_short_fragments: false,
        min_fragment_chars: 1,
        ..ConversionOptions::default()
    };
    let docx_format = SourceFormat::Docx;
    let docx_document = DocxAdapter::default()
        .extract(
            SourceInput {
                path: Some(&docx_path),
                bytes: &docx_bytes,
                format: &docx_format,
            },
            &docx_options,
        )
        .expect("extract DOCX with content splitting disabled");
    assert_eq!(docx_document.stats.split_fragments, 0);
    assert_eq!(docx_document.stats.merged_fragments, 0);
    assert_eq!(docx_document.stats.truncated_titles, 0);
    assert_eq!(docx_document.stats.skipped_items, 2);
    assert_eq!(
        warning_codes(&docx_document.warnings),
        vec![
            ConversionWarningCode::ContentSkipped,
            ConversionWarningCode::ContentSkipped,
        ]
    );

    let merge_bytes = b"# Merge Contract\n\nHi. This second sentence is long enough.";
    let merge_options = ConversionOptions {
        split_content: true,
        preserve_original_paragraphs: true,
        merge_short_fragments: true,
        min_fragment_chars: 12,
        ..corpus_options()
    };
    let markdown_format = SourceFormat::Markdown;
    let merged = MarkdownAdapter
        .extract(
            SourceInput {
                path: None,
                bytes: merge_bytes,
                format: &markdown_format,
            },
            &merge_options,
        )
        .expect("extract merge contract");
    assert_eq!(merged.stats.split_fragments, 1);
    assert_eq!(merged.stats.merged_fragments, 1);
    assert_eq!(merged.stats.truncated_titles, 0);
    assert_eq!(merged.stats.skipped_items, 0);
    assert_eq!(warning_codes(&merged.warnings), Vec::new());

    let truncate_bytes = b"# Truncate Contract\n\n## A heading that must be truncated";
    let truncate_options = ConversionOptions {
        split_content: true,
        preserve_original_paragraphs: true,
        merge_short_fragments: false,
        min_fragment_chars: 1,
        max_title_chars: 16,
        ..ConversionOptions::default()
    };
    let truncated = MarkdownAdapter
        .extract(
            SourceInput {
                path: None,
                bytes: truncate_bytes,
                format: &markdown_format,
            },
            &truncate_options,
        )
        .expect("extract truncation contract");
    assert_eq!(truncated.stats.split_fragments, 0);
    assert_eq!(truncated.stats.merged_fragments, 0);
    assert_eq!(truncated.stats.truncated_titles, 1);
    assert_eq!(truncated.stats.skipped_items, 0);
    assert_eq!(
        warning_codes(&truncated.warnings),
        vec![ConversionWarningCode::TitleTruncated]
    );
}

#[test]
fn task_state_writes_top_level_todo_fields_and_preserves_notes() {
    let manifest = load_manifest();
    let options = corpus_options();
    let markdown = document(&manifest, "markdown-normal.md");
    let written = write_xmind_bytes(&markdown, &options).expect("write Markdown tasks");
    let mut package =
        XmindPackage::from_bytes(written.bytes).expect("open generated Markdown package");
    let root = &package.content()[0]["rootTopic"];

    let todo = find_xmind_topic(root, "Build adapters").expect("todo topic");
    assert_todo_topic(todo, "todo", 0);
    assert!(xmind_plain_note(todo).is_some_and(|note| note.contains("Task status: todo")));
    let done = find_xmind_topic(root, "Markdown").expect("done topic");
    assert_todo_topic(done, "done", 1);
    assert!(xmind_plain_note(done).is_some_and(|note| note.contains("Task status: done")));

    let path = temp_path("todo-task-fields.xmind");
    package
        .save_as(&path)
        .expect("save generated Markdown tasks");
    let reopened = XmindPackage::open(&path).expect("reopen saved Markdown tasks");
    let root = &reopened.content()[0]["rootTopic"];
    assert_todo_topic(
        find_xmind_topic(root, "Build adapters").expect("reopened todo topic"),
        "todo",
        0,
    );
    assert_todo_topic(
        find_xmind_topic(root, "Markdown").expect("reopened done topic"),
        "done",
        1,
    );
    let _ = fs::remove_file(path);

    let opml = document(&manifest, "opml-normal.opml");
    let written = write_xmind_bytes(&opml, &options).expect("write OPML tasks");
    let package = XmindPackage::from_bytes(written.bytes).expect("open generated OPML package");
    let root = &package.content()[0]["rootTopic"];
    assert_todo_topic(
        find_xmind_topic(root, "Platforms").expect("OPML todo topic"),
        "todo",
        0,
    );
    assert_todo_topic(
        find_xmind_topic(root, "macOS").expect("OPML done topic"),
        "done",
        1,
    );
}

#[test]
fn normal_corpus_tree_projection_matches_cross_platform_golden() {
    let manifest = load_manifest();
    let expected_dir = corpus_root()
        .parent()
        .expect("corpus parent")
        .join("expected");
    fs::create_dir_all(&expected_dir).expect("create golden directory");
    let update = std::env::var_os("UPDATE_MINDMAP_TREE_GOLDENS").is_some();

    for entry in &manifest.fixtures {
        if entry.role != "normal" {
            continue;
        }
        let document = document(&manifest, &entry.path);
        let mut projection = serde_json::to_value(&document).expect("serialize tree projection");
        strip_source_locations(&mut projection);
        let mut expected =
            serde_json::to_vec_pretty(&projection).expect("serialize pretty tree projection");
        expected.push(b'\n');
        let golden_path = expected_dir.join(format!("{}.tree.json", entry.path));

        if update {
            fs::write(&golden_path, &expected).expect("write tree golden");
            continue;
        }

        let actual = fs::read(&golden_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", golden_path.display()));
        assert_eq!(
            actual, expected,
            "cross-platform tree drift for {}; rerun with UPDATE_MINDMAP_TREE_GOLDENS=1 only after reviewing the change",
            entry.path
        );
    }
}

#[test]
fn edge_corpus_reports_degradation_and_still_produces_valid_xmind() {
    let manifest = load_manifest();
    let options = corpus_options();

    let markdown = document(&manifest, "markdown-edge.md");
    assert_eq!(markdown.title, "Markdown Edge Corpus");
    assert!(has_warning(
        &markdown,
        ConversionWarningCode::UnsupportedElement,
        "Raw Markdown HTML"
    ));
    assert!(
        all_nodes(&markdown)
            .iter()
            .any(|node| node.title.contains("v1.2.3"))
    );

    let docx = document(&manifest, "docx-edge.docx");
    assert_eq!(docx.title, "DOCX Edge Corpus");
    assert!(has_warning(
        &docx,
        ConversionWarningCode::UnsupportedElement,
        "comment"
    ));
    assert!(has_warning(
        &docx,
        ConversionWarningCode::UnsupportedElement,
        "footnote"
    ));
    assert!(has_warning(
        &docx,
        ConversionWarningCode::UnsupportedElement,
        "MysteryStyle"
    ));
    assert_eq!(docx.table_count(), 1);
    assert_eq!(docx.attachment_count(), 1);
    assert!(docx.link_count() >= 1);

    let html = document(&manifest, "html-edge.html");
    assert_eq!(html.title, "HTML Edge Corpus");
    assert!(has_warning(
        &html,
        ConversionWarningCode::UnsupportedElement,
        "Ignored <nav>"
    ));
    assert!(has_warning(
        &html,
        ConversionWarningCode::UnsupportedElement,
        "Container <custom>"
    ));

    let opml = document(&manifest, "opml-edge.opml");
    assert_eq!(opml.title, "OPML Edge Corpus");
    assert!(has_warning(
        &opml,
        ConversionWarningCode::UnsupportedElement,
        "non-outline"
    ));
    assert!(has_warning(
        &opml,
        ConversionWarningCode::UnsupportedElement,
        "unsupported OPML element"
    ));
    assert_eq!(top(&opml, "Kept").children[0].title, "Child");

    let text = document(&manifest, "text-edge.txt");
    assert!(text.source.original_format["mixed_indentation"] == json!(true));
    assert!(has_warning(
        &text,
        ConversionWarningCode::InvalidStructure,
        "mixes tab and space"
    ));
    assert!(has_warning(
        &text,
        ConversionWarningCode::InvalidStructure,
        "looked like headings"
    ));
    assert!(
        all_nodes(&text)
            .iter()
            .any(|node| node.title.starts_with("Paragraph "))
    );

    for path in [
        "markdown-edge.md",
        "docx-edge.docx",
        "html-edge.html",
        "opml-edge.opml",
        "text-edge.txt",
    ] {
        let document = document(&manifest, path);
        let written = write_xmind_bytes(&document, &options)
            .unwrap_or_else(|error| panic!("write {path}: {error}"));
        assert!(
            written.report.warnings.len() >= document.warnings.len(),
            "writer report dropped adapter warnings for {path}"
        );
        XmindPackage::from_bytes(written.bytes)
            .unwrap_or_else(|error| panic!("reopen generated {path}: {error}"));
    }
}

#[test]
fn malformed_corpus_fails_explicitly_without_producing_a_document() {
    let manifest = load_manifest();
    for path in [
        "markdown-malformed.md",
        "docx-malformed.docx",
        "html-malformed.html",
        "opml-malformed.opml",
        "text-malformed.txt",
    ] {
        let error = match extract_fixture(fixture_entry(&manifest, path)) {
            Ok(_) => panic!("malformed fixture {path} unexpectedly converted"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(!message.trim().is_empty(), "empty error for {path}");
    }
}

#[test]
fn generated_package_preserves_unknown_json_and_zip_entries_after_edit() {
    let manifest = load_manifest();
    let document = document(&manifest, "markdown-normal.md");
    let options = corpus_options();
    let generated = write_xmind_bytes(&document, &options).expect("write generated package");
    let augmented = add_unknown_fields_and_entry(&generated.bytes);
    let mut package = XmindPackage::from_bytes(augmented).expect("open augmented package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["title"] = json!("Edited Import Corpus");
            content[0]["futureEditField"] = json!({"kept": true});
            Ok(())
        })
        .expect("edit augmented package");

    let path = temp_path("unknown-field-safe.xmind");
    package.save_as(&path).expect("save augmented package");
    let saved_bytes = fs::read(&path).expect("read saved package");
    let saved = XmindPackage::open(&path).expect("reopen saved package");

    assert_eq!(
        saved.content()[0]["rootTopic"]["title"],
        json!("Edited Import Corpus")
    );
    assert_eq!(
        saved.content()[0]["futureImportField"],
        json!({"kept": true})
    );
    assert_eq!(
        saved.content()[0]["rootTopic"]["futureTopicField"],
        json!(["a", "b"])
    );
    assert_eq!(saved.content()[0]["futureEditField"], json!({"kept": true}));
    assert_eq!(
        read_zip_entry(&saved_bytes, "resources/future-import.bin"),
        b"\0future\0".to_vec()
    );

    let _ = fs::remove_file(path);
}

#[test]
fn large_synthetic_import_matches_the_bounded_performance_baseline() {
    let manifest = load_manifest();
    let baseline = &manifest.performance_sanity;
    let input = synthetic_markdown(baseline.synthetic_topics);
    let options = ConversionOptions {
        min_fragment_chars: 1,
        max_nodes: baseline.synthetic_topics + 10,
        ..ConversionOptions::default()
    };
    let format = SourceFormat::Markdown;
    let source = SourceInput {
        path: Some(Path::new("synthetic-large.md")),
        bytes: input.as_bytes(),
        format: &format,
    };

    let started = Instant::now();
    let document = MarkdownAdapter
        .extract(source, &options)
        .expect("extract synthetic large Markdown");
    assert_eq!(
        document.node_count(),
        baseline.expected_outline_nodes,
        "synthetic outline node count drifted"
    );
    let first = write_xmind_bytes(&document, &options).expect("write synthetic package");
    let second = write_xmind_bytes(&document, &options).expect("rewrite synthetic package");
    let elapsed = started.elapsed();
    assert_eq!(
        first.bytes, second.bytes,
        "large output must be deterministic"
    );
    assert!(
        first.bytes.len() <= baseline.max_package_bytes,
        "package grew to {} bytes (baseline cap {})",
        first.bytes.len(),
        baseline.max_package_bytes
    );
    assert!(
        elapsed <= Duration::from_millis(baseline.max_elapsed_ms),
        "synthetic import took {elapsed:?} (baseline {:?})",
        Duration::from_millis(baseline.max_elapsed_ms)
    );
    let package = XmindPackage::from_bytes(first.bytes).expect("reopen synthetic package");
    assert_eq!(
        topic_count(&package.content()[0]["rootTopic"]),
        baseline.expected_package_topics
    );
}

fn xmind_topic_children(topic: &Value) -> &[Value] {
    topic
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn find_xmind_topic<'a>(topic: &'a Value, title: &str) -> Option<&'a Value> {
    if topic.get("title").and_then(Value::as_str) == Some(title) {
        return Some(topic);
    }
    xmind_topic_children(topic)
        .iter()
        .find_map(|child| find_xmind_topic(child, title))
}

fn assert_todo_topic(topic: &Value, status: &str, progress: i64) {
    assert_eq!(topic["taskPresentation"], json!("todo-checkbox"));
    assert_eq!(topic["todoStatus"], json!(status));
    assert_eq!(topic["todoProgress"], json!(progress));
}

fn xmind_plain_note(topic: &Value) -> Option<&str> {
    topic
        .get("notes")
        .and_then(|notes| notes.get("plain"))
        .and_then(|plain| plain.get("content"))
        .and_then(Value::as_str)
}

fn warning_codes(warnings: &[ConversionWarning]) -> Vec<ConversionWarningCode> {
    warnings
        .iter()
        .map(|warning| warning.code.clone())
        .collect()
}

fn assert_xmind_round_trip(
    path: &str,
    document: &OutlineDocument,
    options: &ConversionOptions,
    expected_writer_warnings: &[ConversionWarningCode],
) {
    let first = write_xmind_bytes(document, options)
        .unwrap_or_else(|error| panic!("write {path}: {error}"));
    let second = write_xmind_bytes(document, options)
        .unwrap_or_else(|error| panic!("rewrite {path}: {error}"));
    assert_eq!(
        first.bytes, second.bytes,
        "non-deterministic output for {path}"
    );
    assert_eq!(first.report, second.report, "report drift for {path}");
    assert_eq!(
        warning_codes(&first.report.warnings),
        expected_writer_warnings,
        "writer warning code/order drift for {path}"
    );
    assert_eq!(
        &first.report.warnings[..document.warnings.len()],
        document.warnings.as_slice(),
        "adapter warnings were reordered or dropped for {path}"
    );

    let augmented = add_unknown_fields_and_entry(&first.bytes);
    let mut package = XmindPackage::from_bytes(augmented)
        .unwrap_or_else(|error| panic!("open augmented {path}: {error}"));
    package
        .edit_content_json(|content| {
            content[0]["futureRoundTripField"] = json!({ "fixture": path });
            Ok(())
        })
        .expect("add round-trip marker");
    let expected_content = package.content().clone();

    let output = temp_path(&format!("{path}-roundtrip.xmind"));
    let save_report = package
        .save_as(&output)
        .unwrap_or_else(|error| panic!("save {path}: {error}"));
    assert_eq!(
        save_report.rewritten_entries,
        vec![CONTENT_JSON_ENTRY.to_string()]
    );
    let saved =
        XmindPackage::open(&output).unwrap_or_else(|error| panic!("reopen saved {path}: {error}"));
    assert_eq!(
        saved.content(),
        &expected_content,
        "structure/notes/links/tables/unknown fields drifted for {path}"
    );
    assert_eq!(
        saved.content()[0]["futureImportField"],
        json!({ "kept": true })
    );
    assert_eq!(
        saved.content()[0]["rootTopic"]["futureTopicField"],
        json!(["a", "b"])
    );
    assert_eq!(
        saved.content()[0]["futureRoundTripField"],
        json!({ "fixture": path })
    );
    assert_eq!(
        read_zip_entry(
            &fs::read(&output).expect("read saved round-trip"),
            "resources/future-import.bin"
        ),
        b"\0future\0".to_vec()
    );

    let _ = fs::remove_file(output);
}

fn synthetic_markdown(topic_count: usize) -> String {
    let mut output = String::with_capacity(topic_count * 80);
    output.push_str("# Synthetic Import Performance\n\n");
    for index in 0..topic_count {
        output.push_str(&format!(
            "- Topic {index:04} stays deterministic with a useful amount of text.\n"
        ));
    }
    output
}

fn strip_source_locations(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("source");
            for child in object.values_mut() {
                strip_source_locations(child);
            }
        }
        Value::Array(array) => {
            for child in array {
                strip_source_locations(child);
            }
        }
        _ => {}
    }
}

fn add_unknown_fields_and_entry(bytes: &[u8]) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes.to_vec())).expect("open generated ZIP");
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(
                zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
                    .expect("valid fixed ZIP timestamp"),
            )
            .unix_permissions(0o644);
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).expect("read generated ZIP entry");
            let name = entry.name().to_string();
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .expect("read generated ZIP payload");
            if name == CONTENT_JSON_ENTRY {
                let mut content: Value = serde_json::from_slice(&data).expect("parse content.json");
                content[0]["futureImportField"] = json!({"kept": true});
                content[0]["rootTopic"]["futureTopicField"] = json!(["a", "b"]);
                data = serde_json::to_vec(&content).expect("serialize augmented content.json");
            }
            writer
                .start_file(&name, options)
                .expect("start augmented ZIP entry");
            writer
                .write_all(&data)
                .expect("write augmented ZIP payload");
        }
        writer
            .start_file("resources/future-import.bin", options)
            .expect("start unknown ZIP entry");
        writer
            .write_all(b"\0future\0")
            .expect("write unknown ZIP payload");
        writer.finish().expect("finish augmented ZIP");
    }
    output.into_inner()
}

fn read_zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes.to_vec())).expect("open saved ZIP");
    let mut entry = archive.by_name(name).expect("find saved ZIP entry");
    let mut output = Vec::new();
    entry
        .read_to_end(&mut output)
        .expect("read saved ZIP entry");
    output
}

fn topic_count(topic: &Value) -> usize {
    1 + topic
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)
        .map(|children| children.iter().map(topic_count).sum())
        .unwrap_or(0)
}

fn temp_path(label: &str) -> PathBuf {
    let id = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/mindmap-import-tests");
    fs::create_dir_all(&directory).expect("create test temp directory");
    directory.join(format!(
        "{}-{}-{id}-{label}",
        std::process::id(),
        unix_timestamp()
    ))
}

fn unix_timestamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos()
}
