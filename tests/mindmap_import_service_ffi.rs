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

fn markdown_fixture(root: &Path) -> PathBuf {
    let path = root.join("release-notes.md");
    fs::write(
        &path,
        "# Kite 发布说明\n\n## 转换能力\n正文第一句。正文第二句；正文第三句。\n\n### 表格\n| 项目 | 状态 |\n| --- | --- |\n| Markdown | 完成 |\n\n### 代码\n```rust\nfn main() { println!(\"hi\"); }\n```\n\n![架构图](https://example.com/architecture.png)\n",
    )
    .expect("write markdown");
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
