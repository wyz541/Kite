use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_ffi::{
    MindmapConnectorKindDto, MindmapDateKindDto, MindmapDatePartOrderDto, MindmapDatePlacementDto,
    MindmapDatePrecisionDto, MindmapDateRequestDto, MindmapDateStyleDto, MindmapDateTargetDto,
    MindmapDocumentStylePatchDto, MindmapGraphStructureDto, MindmapTodoStatusDto,
    MindmapTopicStyleDto, add_mindmap_marker, insert_mindmap_date, open_mindmap_document,
    read_mindmap_document_style, read_mindmap_topic_markers, read_mindmap_topic_style,
    read_mindmap_topic_todo, set_mindmap_priority, set_mindmap_task_progress, toggle_mindmap_todo,
    update_mindmap_document_style, update_mindmap_topic_style,
};
use serde_json::{Value, json};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn ffi_format_commands_save_and_preserve_unknown_package_data() {
    let path = test_path("format-commands");
    fs::write(&path, synthetic_package()).expect("write synthetic package");
    let mut document = open_mindmap_document(path.display().to_string()).expect("open document");

    let style = read_mindmap_document_style(path.display().to_string(), 0).expect("read style");
    assert_eq!(style.structure, MindmapGraphStructureDto::LogicRight);

    document = update_mindmap_document_style(
        path.display().to_string(),
        document.source_sha256,
        0,
        MindmapDocumentStylePatchDto {
            structure: MindmapGraphStructureDto::MindMap,
            connector_kind: MindmapConnectorKindDto::RoundedElbow,
            connector_corner_radius: Some(18),
            connector_width: Some("3pt".to_string()),
            connector_color: Some("#7FB4FFAA".to_string()),
            connector_pattern: Some("dash".to_string()),
        },
    )
    .expect("update document style");

    document = set_mindmap_priority(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        2,
    )
    .expect("set priority");
    document = set_mindmap_task_progress(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        "task-half".to_string(),
    )
    .expect("set progress");
    document = add_mindmap_marker(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        "flag-red".to_string(),
    )
    .expect("add marker");
    document = toggle_mindmap_todo(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
    )
    .expect("toggle todo");
    document = insert_mindmap_date(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        MindmapDateRequestDto {
            date: "2026-11-27".to_string(),
            time: Some("14:05:09".to_string()),
            style: MindmapDateStyleDto::Iso,
            precision: MindmapDatePrecisionDto::Seconds,
            target: MindmapDateTargetDto::Title,
            placement: MindmapDatePlacementDto::End,
            kind: MindmapDateKindDto::Date,
            system_date_order: None,
            system_date_separator: None,
        },
    )
    .map(|result| result.document)
    .expect("insert date");
    let _document = update_mindmap_topic_style(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        MindmapTopicStyleDto {
            font_family: Some("JetBrains Mono".to_string()),
            font_size: Some(14.0),
            font_weight: Some("bold".to_string()),
            italic: Some(true),
            underline: Some(true),
            strike: Some(false),
            text_color: Some("#112233".to_string()),
            alignment: Some("left".to_string()),
            line_height: Some("1.4".to_string()),
            letter_spacing: Some("0.25pt".to_string()),
            node_fill: Some("#FAFAFA".to_string()),
            border_color: Some("#445566".to_string()),
            border_width: Some(2.0),
            border_pattern: Some("dash".to_string()),
            corner_radius: Some(9),
        },
    )
    .expect("update topic style");

    let todo = read_mindmap_topic_todo(path.display().to_string(), 0, "target".to_string())
        .expect("read todo");
    assert_eq!(todo.status, MindmapTodoStatusDto::Done);
    let markers = read_mindmap_topic_markers(path.display().to_string(), 0, "target".to_string())
        .expect("read markers");
    assert_eq!(markers.priority_marker_id.as_deref(), Some("priority-2"));
    assert_eq!(
        markers.task_progress_marker_id.as_deref(),
        Some("task-done")
    );
    let local_style = read_mindmap_topic_style(path.display().to_string(), 0, "target".to_string())
        .expect("read topic style");
    assert_eq!(local_style.font_family.as_deref(), Some("JetBrains Mono"));
    assert_eq!(local_style.font_size, Some(14.0));
    assert_eq!(local_style.corner_radius, Some(9));
    assert_eq!(local_style.line_height.as_deref(), Some("1.4"));
    assert_eq!(local_style.letter_spacing.as_deref(), Some("0.25pt"));

    let package = XmindPackage::open(&path).expect("reopen edited package");
    let root = &package.content()[0]["rootTopic"];
    assert_eq!(root["structureClass"], "org.xmind.ui.map");
    assert_eq!(
        root["style"]["properties"]["line-class"],
        "org.xmind.branchConnection.roundedElbow"
    );
    assert_eq!(root["style"]["properties"]["line-corner"], "18");
    assert_eq!(root["style"]["properties"]["line-width"], "3pt");
    assert_eq!(root["style"]["properties"]["line-color"], "#7FB4FFAA");
    assert_eq!(root["style"]["properties"]["line-pattern"], "dash");
    let topic = topic_by_id(root, "target").expect("target topic");
    assert_eq!(topic["title"], "Target 2026-11-27 14:05:09");
    assert_eq!(topic["futureTopicField"]["keep"], true);
    assert_eq!(
        topic["style"]["properties"]["fo:font-family"],
        "JetBrains Mono"
    );
    assert_eq!(topic["style"]["properties"]["fo:font-size"], "14pt");
    assert_eq!(topic["style"]["properties"]["fo:font-weight"], "bold");
    assert_eq!(topic["style"]["properties"]["fo:font-style"], "italic");
    assert_eq!(topic["style"]["properties"]["fo:color"], "#112233");
    assert_eq!(topic["style"]["properties"]["fo:line-height"], "1.4");
    assert_eq!(topic["style"]["properties"]["fo:letter-spacing"], "0.25pt");
    assert_eq!(topic["style"]["properties"]["svg:fill"], "#FAFAFA");
    assert_eq!(topic["style"]["properties"]["border-line-width"], "2pt");
    assert_eq!(topic["style"]["properties"]["shape-corner"], "9");
    assert!(
        topic["markers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["markerId"] == "priority-2" })
    );
    assert!(
        topic["markers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["markerId"] == "task-done" })
    );
    assert!(
        topic["markers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["markerId"] == "flag-red" })
    );

    let bytes = fs::read(&path).expect("read saved package");
    assert_eq!(
        read_zip_entry(&bytes, "resources/keep.bin"),
        b"future-package"
    );
    let _ = fs::remove_file(path);
}

#[test]
fn unmodified_ffi_open_preserves_source_hash_and_bytes() {
    let path = test_path("unchanged");
    let original = synthetic_package();
    fs::write(&path, &original).expect("write synthetic package");
    let document = open_mindmap_document(path.display().to_string()).expect("open document");
    assert_eq!(fs::read(&path).expect("read source"), original);
    assert_eq!(document.sheet_count, 1);
    let _ = fs::remove_file(path);
}

#[test]
fn ffi_todo_dto_consumes_xmind_271_top_level_task_fields() {
    let path = test_path("modern-task-fields");
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["children"]["attached"][0]["taskPresentation"] =
                json!("todo-progress");
            content[0]["rootTopic"]["children"]["attached"][0]["todoStatus"] = json!("done");
            content[0]["rootTopic"]["children"]["attached"][0]["todoProgress"] = json!(1.0);
            Ok(())
        })
        .expect("seed modern task fields");
    package.save_as(&path).expect("save modern task package");

    let todo = read_mindmap_topic_todo(path.display().to_string(), 0, "target".to_string())
        .expect("read modern task fields");
    assert_eq!(todo.task_presentation.as_deref(), Some("todo-progress"));
    assert_eq!(todo.todo_status.as_deref(), Some("done"));
    assert_eq!(todo.todo_progress, Some(1.0));
    assert!(todo.is_complete);
    let _ = fs::remove_file(path);
}

#[test]
fn system_date_and_textual_due_intent_are_explicit() {
    let path = test_path("system-date-due");
    fs::write(&path, synthetic_package()).expect("write synthetic package");
    let mut document = open_mindmap_document(path.display().to_string()).expect("open document");

    let result = insert_mindmap_date(
        path.display().to_string(),
        document.source_sha256.clone(),
        0,
        "target".to_string(),
        MindmapDateRequestDto {
            date: "2026-11-27".to_string(),
            time: None,
            style: MindmapDateStyleDto::System,
            precision: MindmapDatePrecisionDto::DateOnly,
            target: MindmapDateTargetDto::Title,
            placement: MindmapDatePlacementDto::End,
            kind: MindmapDateKindDto::Date,
            system_date_order: Some(MindmapDatePartOrderDto::DayMonthYear),
            system_date_separator: Some("/".to_string()),
        },
    )
    .expect("insert system date");
    document = result.document;
    assert_eq!(result.text, "27/11/2026");

    let result = insert_mindmap_date(
        path.display().to_string(),
        document.source_sha256,
        0,
        "target".to_string(),
        MindmapDateRequestDto {
            date: "2026-12-01".to_string(),
            time: None,
            style: MindmapDateStyleDto::Iso,
            precision: MindmapDatePrecisionDto::DateOnly,
            target: MindmapDateTargetDto::Note,
            placement: MindmapDatePlacementDto::Start,
            kind: MindmapDateKindDto::Due,
            system_date_order: None,
            system_date_separator: None,
        },
    )
    .expect("insert textual due date");
    assert_eq!(result.text, "截止 2026-12-01");
    let package = XmindPackage::open(&path).expect("reopen edited package");
    let target = topic_by_id(&package.content()[0]["rootTopic"], "target").expect("target");
    assert_eq!(target["title"], "Target 27/11/2026");
    assert_eq!(target["notes"]["plain"]["content"], "截止 2026-12-01");
    let _ = fs::remove_file(path);
}

fn synthetic_package() -> Vec<u8> {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "title": "FFI format fixture",
            "rootTopic": {
                "id": "root",
                "class": "topic",
                "title": "Root",
                "structureClass": "org.xmind.ui.logic.right",
                "style": {
                    "id": "root-style",
                    "type": "topic",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.curve",
                        "futureConnectorProperty": {"keep": true}
                    }
                },
                "children": {
                    "attached": [
                        {
                            "id": "target",
                            "class": "topic",
                            "title": "Target",
                            "futureTopicField": {"keep": true},
                            "markers": [
                                {"markerId": "priority-1", "futureMarkerField": {"keep": true}},
                                {"markerId": "future-marker"}
                            ],
                            "style": {
                                "id": "target-style",
                                "type": "topic",
                                "properties": {
                                    "futureStyleProperty": "keep",
                                    "fo:font-size": "12pt"
                                }
                            }
                        }
                    ]
                }
            }
        }
    ]);
    zip_bytes(&[
        (
            "content.json",
            serde_json::to_vec(&content).expect("serialize content"),
        ),
        (
            "metadata.json",
            br#"{"activeSheetId":"sheet-1","futureMetadata":true}"#.to_vec(),
        ),
        ("resources/keep.bin", b"future-package".to_vec()),
    ])
}

fn zip_bytes(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("start ZIP entry");
            writer.write_all(bytes).expect("write ZIP entry");
        }
        writer.finish().expect("finish ZIP");
    }
    output.into_inner()
}

fn read_zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("read ZIP");
    let mut entry = archive.by_name(name).expect("find ZIP entry");
    let mut result = Vec::new();
    entry.read_to_end(&mut result).expect("read ZIP entry");
    result
}

fn topic_by_id<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    if value.get("id").and_then(Value::as_str) == Some(id) {
        return Some(value);
    }
    value
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)?
        .iter()
        .find_map(|child| topic_by_id(child, id))
}

fn test_path(label: &str) -> PathBuf {
    let sequence = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kite-mindmap-ffi-{label}-{}-{sequence}.xmind",
        std::process::id()
    ))
}
