use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_ffi::{
    MindmapConversionOptionsDto, MindmapImportCodeStrategyDto, MindmapImportImageStrategyDto,
    MindmapImportPresetDto, MindmapImportTableStrategyDto, convert_mindmap_file,
    detect_mindmap_import_source, preview_mindmap_conversion, supported_mindmap_import_formats,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn temp_dir(label: &str) -> PathBuf {
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "kite-import-ui-{label}-{}-{nanos}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temp directory");
    path
}

fn rows_table_fixture(root: &Path) -> PathBuf {
    let path = root.join("rows-table.html");
    fs::write(
        &path,
        "<html><head><title>Rows Table</title></head><body><h1>Details</h1><table><caption>Matrix</caption><tr><th>Platform</th><th>Status</th></tr><tr><td>macOS</td><td>Done</td></tr><tr><td>Windows</td><td>Pending</td></tr></table></body></html>",
    )
    .expect("write HTML table");
    path
}

fn topic_children(topic: &serde_json::Value) -> &[serde_json::Value] {
    topic
        .get("children")
        .and_then(|children| {
            children.as_array().or_else(|| {
                children
                    .get("attached")
                    .and_then(serde_json::Value::as_array)
            })
        })
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn plain_note(topic: &serde_json::Value) -> Option<&str> {
    topic
        .get("notes")
        .and_then(|notes| notes.get("plain"))
        .and_then(|plain| plain.get("content"))
        .and_then(serde_json::Value::as_str)
}

fn find_topic_by_kind<'a>(
    topic: &'a serde_json::Value,
    kind: &str,
) -> Option<&'a serde_json::Value> {
    if topic.get("kind").and_then(serde_json::Value::as_str) == Some(kind) {
        return Some(topic);
    }
    topic_children(topic)
        .iter()
        .find_map(|child| find_topic_by_kind(child, kind))
}

fn find_topic_with_note<'a>(
    topic: &'a serde_json::Value,
    expected_note: &str,
) -> Option<&'a serde_json::Value> {
    if plain_note(topic) == Some(expected_note) {
        return Some(topic);
    }
    topic_children(topic)
        .iter()
        .find_map(|child| find_topic_with_note(child, expected_note))
}

fn markdown_fixture(root: &Path) -> PathBuf {
    let path = root.join("release-notes.md");
    fs::write(
        &path,
        "# Kite 发布说明\n\n## 转换能力\n正文第一句。正文第二句；正文第三句。\n\n### 表格\n| 项目 | 状态 |\n| --- | --- |\n| Markdown | 完成 |\n\n### 代码\n```rust\nfn main() { println!(\"hi\"); }\n```\n\n![架构图](https://example.com/architecture.png)\n",
    )
    .expect("write markdown");
    path
}

fn low_confidence_text_fixture(root: &Path) -> PathBuf {
    let path = root.join("ambiguous-notes.txt");
    fs::write(
        &path,
        "背景\n下一句正文需要足够长才能稳定识别吗。\n\n第二段正文同样足够长，不会触发短句合并。\n",
    )
    .expect("write text");
    path
}

#[test]
fn format_catalog_and_detection_match_supported_adapters() {
    let formats = supported_mindmap_import_formats();
    let ids = formats
        .iter()
        .map(|format| format.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"markdown"));
    assert!(ids.contains(&"text"));
    assert!(ids.contains(&"docx"));
    assert!(ids.contains(&"html"));
    assert!(ids.contains(&"opml"));

    let markdown_path = std::env::temp_dir().join("a.MD");
    let markdown = detect_mindmap_import_source(markdown_path.to_string_lossy().into_owned())
        .expect("markdown");
    assert_eq!(markdown.format.id, "markdown");
    assert!(markdown.format.supported);
    assert_eq!(
        Path::new(&markdown.default_output_path),
        markdown_path.with_extension("xmind")
    );

    let text_path = std::env::temp_dir().join("notes.txt");
    let text =
        detect_mindmap_import_source(text_path.to_string_lossy().into_owned()).expect("text");
    assert_eq!(text.format.id, "text");
    assert!(text.format.supported);

    let pdf_path = std::env::temp_dir().join("report.pdf");
    let pdf =
        detect_mindmap_import_source(pdf_path.to_string_lossy().into_owned()).expect("pdf known");
    assert!(!pdf.format.supported);
    for unsupported in ["map.xmind", "unknown.bin"] {
        let path = std::env::temp_dir().join(unsupported);
        assert!(detect_mindmap_import_source(path.to_string_lossy().into_owned()).is_none());
    }
}

#[test]
fn markdown_preview_and_conversion_are_safe_and_openable() {
    let root = temp_dir("markdown-roundtrip");
    let source_path = markdown_fixture(&root);
    let original = fs::read(&source_path).expect("read source");

    let preview = preview_mindmap_conversion(
        source_path.to_string_lossy().into_owned(),
        MindmapConversionOptionsDto {
            preset: MindmapImportPresetDto::SentenceSplit,
            split_content: true,
            merge_short_fragments: false,
            table_strategy: MindmapImportTableStrategyDto::RowsAsTopics,
            code_strategy: MindmapImportCodeStrategyDto::ParentNote,
            image_strategy: MindmapImportImageStrategyDto::ParentNote,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect("preview");
    assert_eq!(preview.source.format.id, "markdown");
    assert!(preview.node_count >= 5);
    assert!(preview.sampled_node_count >= 4);
    assert_eq!(preview.source_sha256.len(), 64);
    let sample: serde_json::Value =
        serde_json::from_str(&preview.sample_tree_json).expect("preview JSON");
    assert_eq!(sample["kind"], "document");
    assert!(
        sample["children"]
            .as_array()
            .is_some_and(|children| !children.is_empty())
    );
    assert!(preview.output_path.ends_with(".xmind"));
    assert_ne!(preview.output_path, source_path.to_string_lossy());

    let result = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256.clone(),
        preview.output_path.clone(),
        false,
        MindmapConversionOptionsDto {
            preset: MindmapImportPresetDto::SentenceSplit,
            split_content: true,
            merge_short_fragments: false,
            table_strategy: MindmapImportTableStrategyDto::RowsAsTopics,
            code_strategy: MindmapImportCodeStrategyDto::ParentNote,
            image_strategy: MindmapImportImageStrategyDto::ParentNote,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect("convert");

    assert!(result.source_unchanged);
    assert_eq!(fs::read(&source_path).expect("source after"), original);
    assert!(Path::new(&result.output_path).is_file());
    assert_eq!(result.output_sha256.len(), 64);
    let package = XmindPackage::open(Path::new(&result.output_path)).expect("generated XMind");
    assert_eq!(package.summary().sheet_count, 1);
    assert!(
        serde_json::to_string(package.content())
            .expect("content")
            .contains("Kite 发布说明")
    );

    let output_before = fs::read(&result.output_path).expect("output before retry");
    let error = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        result.source_sha256,
        result.output_path,
        false,
        MindmapConversionOptionsDto::default(),
    )
    .expect_err("existing output requires explicit overwrite");
    assert!(error.to_string().contains("已存在"));
    assert_eq!(
        fs::read(root.join("release-notes.xmind")).expect("output after retry"),
        output_before
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn rows_as_topics_expands_each_data_row_with_header_and_caption_context() {
    let root = temp_dir("rows-as-topics");
    let source_path = rows_table_fixture(&root);
    let options = MindmapConversionOptionsDto {
        table_strategy: MindmapImportTableStrategyDto::RowsAsTopics,
        ..MindmapConversionOptionsDto::default()
    };

    let preview =
        preview_mindmap_conversion(source_path.to_string_lossy().into_owned(), options.clone())
            .expect("preview rows as topics");
    assert_eq!(preview.table_count, 0);
    assert_eq!(
        preview
            .warnings
            .iter()
            .filter(|warning| warning.code == "TableFlattened")
            .count(),
        1
    );
    assert!(preview.warnings.iter().any(|warning| {
        warning.code == "TableFlattened"
            && warning.message.contains("one topic per data row")
            && warning.message.contains("header/caption context")
    }));

    let sample: serde_json::Value =
        serde_json::from_str(&preview.sample_tree_json).expect("preview JSON");
    let sampled_table = find_topic_by_kind(&sample, "table").expect("sampled table topic");
    assert_eq!(sampled_table["childCount"], 2);
    assert_eq!(
        topic_children(sampled_table)
            .iter()
            .map(|child| child["title"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["macOS | Done", "Windows | Pending"]
    );

    let result = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256,
        preview.output_path,
        false,
        options,
    )
    .expect("convert rows as topics");
    assert_eq!(
        result
            .warnings
            .iter()
            .filter(|warning| warning.code == "TableFlattened")
            .count(),
        1
    );

    let package = XmindPackage::open(Path::new(&result.output_path)).expect("generated XMind");
    let root_topic = &package.content()[0]["rootTopic"];
    let table = find_topic_with_note(root_topic, "Caption: Matrix\nHeaders: Platform | Status")
        .expect("table topic with caption and header context");
    assert_eq!(
        plain_note(table),
        Some("Caption: Matrix\nHeaders: Platform | Status")
    );
    assert_eq!(
        topic_children(table)
            .iter()
            .map(|child| child["title"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["macOS | Done", "Windows | Pending"]
    );
    assert_eq!(plain_note(&topic_children(table)[0]), Some("macOS | Done"));
    assert_eq!(
        plain_note(&topic_children(table)[1]),
        Some("Windows | Pending")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn preview_exposes_stable_stats_complete_warnings_and_txt_confidence() {
    let root = temp_dir("preview-stats");
    let source_path = low_confidence_text_fixture(&root);
    let preview = preview_mindmap_conversion(
        source_path.to_string_lossy().into_owned(),
        MindmapConversionOptionsDto {
            max_title_chars: 4,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect("text preview");

    assert_eq!(preview.structure_confidence, "low");
    assert_eq!(preview.inferred_heading_count, 0);
    assert!(preview.ambiguous_short_line_count >= 1);
    assert!(preview.split_fragments >= 1);
    assert!(!preview.warnings.is_empty());
    assert!(preview.warnings.iter().all(|warning| {
        !warning.code.is_empty() && !warning.severity.is_empty() && !warning.message.is_empty()
    }));
    assert!(preview.warnings.iter().any(|warning| {
        warning.code == "TitleTruncated"
            && warning
                .location
                .as_deref()
                .is_some_and(|location| location.contains("第 1 行"))
    }));
    assert!(preview.warnings.iter().any(|warning| {
        warning.code == "InvalidStructure" && warning.message.contains("short line")
    }));

    let result = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256.clone(),
        preview.output_path.clone(),
        false,
        MindmapConversionOptionsDto {
            max_title_chars: 4,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect("convert previewed text");
    assert_eq!(result.warnings.len(), preview.warnings.len());
    assert_eq!(
        result
            .warnings
            .iter()
            .map(|warning| warning.code.as_str())
            .collect::<Vec<_>>(),
        preview
            .warnings
            .iter()
            .map(|warning| warning.code.as_str())
            .collect::<Vec<_>>()
    );

    let sample: serde_json::Value =
        serde_json::from_str(&preview.sample_tree_json).expect("preview JSON");
    assert_eq!(sample["kind"], "document");
    assert_eq!(sample["structureConfidence"], "low");
    assert_eq!(sample["inferredHeadingCount"], 0);
    assert_eq!(
        sample["ambiguousShortLineCount"].as_u64(),
        Some(u64::from(preview.ambiguous_short_line_count))
    );
    assert!(
        sample["children"]
            .as_array()
            .is_some_and(|children| !children.is_empty())
    );

    let skipped_source = markdown_fixture(&root);
    let skipped = preview_mindmap_conversion(
        skipped_source.to_string_lossy().into_owned(),
        MindmapConversionOptionsDto {
            split_content: false,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect("strict preview");
    assert!(skipped.skipped_items >= 1);
    assert!(
        skipped
            .warnings
            .iter()
            .any(|warning| warning.code == "ContentSkipped")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn explicit_overwrite_and_failed_generation_preserve_existing_output() {
    let root = temp_dir("overwrite-safety");
    let source_path = markdown_fixture(&root);
    let preview = preview_mindmap_conversion(
        source_path.to_string_lossy().into_owned(),
        MindmapConversionOptionsDto::default(),
    )
    .expect("preview");
    let output_path = preview.output_path.clone();
    let sentinel = b"existing output must survive a failed conversion";
    fs::write(&output_path, sentinel).expect("seed existing output");

    let failure = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256.clone(),
        output_path.clone(),
        true,
        MindmapConversionOptionsDto {
            max_title_chars: 0,
            ..MindmapConversionOptionsDto::default()
        },
    )
    .expect_err("invalid options must fail before writing");
    assert!(failure.to_string().contains("max_title_chars"));
    assert_eq!(fs::read(&output_path).expect("existing output"), sentinel);

    let result = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256,
        output_path.clone(),
        true,
        MindmapConversionOptionsDto::default(),
    )
    .expect("explicit overwrite");
    assert_eq!(Path::new(&result.output_path), Path::new(&output_path));
    assert_ne!(fs::read(&output_path).expect("generated output"), sentinel);
    XmindPackage::open(Path::new(&output_path)).expect("overwritten XMind is valid");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn conversion_rejects_stale_sources_and_invalid_outputs() {
    let root = temp_dir("safety");
    let source_path = markdown_fixture(&root);
    let preview = preview_mindmap_conversion(
        source_path.to_string_lossy().into_owned(),
        MindmapConversionOptionsDto::default(),
    )
    .expect("preview");

    let stale = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        "0".repeat(64),
        root.join("stale.xmind").to_string_lossy().into_owned(),
        false,
        MindmapConversionOptionsDto::default(),
    )
    .expect_err("stale source");
    assert!(stale.to_string().contains("发生变化"));
    assert!(!root.join("stale.xmind").exists());

    let invalid_extension = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256.clone(),
        root.join("output.md").to_string_lossy().into_owned(),
        false,
        MindmapConversionOptionsDto::default(),
    )
    .expect_err("invalid extension");
    assert!(invalid_extension.to_string().contains(".xmind"));

    let missing_directory = convert_mindmap_file(
        source_path.to_string_lossy().into_owned(),
        preview.source_sha256,
        root.join("missing/output.xmind")
            .to_string_lossy()
            .into_owned(),
        false,
        MindmapConversionOptionsDto::default(),
    )
    .expect_err("missing output directory");
    assert!(missing_directory.to_string().contains("输出目录"));

    let _ = fs::remove_dir_all(root);
}
