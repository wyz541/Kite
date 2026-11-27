use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kite_core::mindmap::{
    MarkerEntry, MarkerFamily, MarkerKind, OpacitySemantics, PriorityMarker, SlotUpdate,
    TaskProgress, TodoStatus, TodoTextDecoration, TodoUpdate, TopicMarkerError, TopicMarkerPatch,
    TopicTodoPatch, TopicTodoState, VisualTone, XmindPackage,
};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const UI_FIXTURE: &str = "tests/fixtures/mindmap/kite-rust-gpui-capability.xmind";
const UI_FIXTURE_MANIFEST: &str = "tests/fixtures/mindmap/kite-rust-gpui-capability.manifest.json";
static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn reads_ordered_priority_task_and_generic_markers_without_losing_opaque_data() {
    let package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let markers = package
        .topic_markers(0, "topic-markers")
        .expect("read markers");

    assert_eq!(markers.entries.len(), 7);
    assert_eq!(markers.priority(), Some(PriorityMarker::Priority1));
    assert_eq!(markers.task_progress(), Some(TaskProgress::Half));

    let entries = markers.markers().collect::<Vec<_>>();
    let ids = entries
        .iter()
        .map(|marker| marker.marker_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["priority-1", "tag-red", "task-half", "star-yellow",]
    );

    let priority = entries[0];
    assert_eq!(
        priority.kind,
        MarkerKind::Priority(PriorityMarker::Priority1)
    );
    assert_eq!(
        priority.opaque_fields["futureMarkerField"],
        json!({"kept": "priority-marker"})
    );

    let tag = entries[1];
    assert_eq!(
        tag.kind,
        MarkerKind::Generic {
            family: MarkerFamily::Tag
        }
    );
    assert_eq!(tag.opaque_fields["groupId"], json!("tagMarkers"));
    assert_eq!(tag.opaque_fields["futureTagField"], json!([1, 2, 3]));

    let star = entries[3];
    assert_eq!(
        star.kind,
        MarkerKind::Generic {
            family: MarkerFamily::Star
        }
    );
    assert_eq!(star.opaque_fields["futureStarField"], json!("kept"));

    assert!(matches!(markers.entries[4], MarkerEntry::Opaque(_)));
    assert!(matches!(markers.entries[5], MarkerEntry::Opaque(_)));
    assert!(matches!(markers.entries[6], MarkerEntry::Opaque(_)));

    let header = markers.header();
    assert_eq!(header.markers.len(), 4);
    assert_eq!(header.opaque_entries, 3);
    assert!(!package.is_dirty());
}

#[test]
fn marker_patch_replaces_conflicts_and_preserves_unknown_fields_and_order() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let before_other = find_topic(package.content(), "topic-other").clone();
    let before_task_object =
        package.content()[0]["rootTopic"]["children"]["attached"][0]["task"].clone();
    let before_topic_field =
        package.content()[0]["rootTopic"]["children"]["attached"][0]["futureTopicField"].clone();

    package
        .apply_topic_markers(
            0,
            "topic-markers",
            &TopicMarkerPatch {
                priority: SlotUpdate::Set(PriorityMarker::Priority2),
                task_progress: SlotUpdate::Set(TaskProgress::Done),
                add: vec!["flag-red".to_owned()],
                remove: vec!["tag-red".to_owned()],
            },
        )
        .expect("apply markers");

    let topic = find_topic(package.content(), "topic-markers");
    let markers = topic["markers"].as_array().expect("markers array");
    assert_eq!(
        markers
            .iter()
            .filter_map(|entry| entry.get("markerId").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        vec!["priority-2", "task-done", "star-yellow", "flag-red",]
    );
    assert_eq!(
        markers[0]["futureMarkerField"],
        json!({"kept": "priority-marker"})
    );
    assert_eq!(
        markers[1]["futureProgressField"],
        json!({"kept": "progress-marker"})
    );
    assert_eq!(markers[2]["futureStarField"], json!("kept"));
    assert_eq!(markers[3]["futureOpaque"], json!("kept"));
    assert_eq!(markers[4], json!("unexpected-string-marker"));
    assert_eq!(topic["task"], before_task_object);
    assert_eq!(topic["futureTopicField"], before_topic_field);
    assert_eq!(find_topic(package.content(), "topic-other"), &before_other);
    assert!(package.is_dirty());

    let state = package
        .topic_todo(0, "topic-markers")
        .expect("read updated todo state");
    assert_eq!(state.status, TodoStatus::Done);
}

#[test]
fn marker_edits_save_and_reopen_with_unknown_zip_entries_intact() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .apply_topic_markers(
            0,
            "topic-markers",
            &TopicMarkerPatch {
                priority: SlotUpdate::Set(PriorityMarker::Priority3),
                task_progress: SlotUpdate::Set(TaskProgress::Quarter),
                add: vec!["people-green".to_owned()],
                ..TopicMarkerPatch::default()
            },
        )
        .expect("apply markers");

    let path = test_path("save-reopen");
    package.save_as(&path).expect("save package");
    let saved = XmindPackage::open(&path).expect("reopen package");
    let markers = saved
        .topic_markers(0, "topic-markers")
        .expect("read reopened markers");
    assert_eq!(markers.priority(), Some(PriorityMarker::Priority3));
    assert_eq!(markers.task_progress(), Some(TaskProgress::Quarter));
    assert!(markers.markers().any(|marker| {
        marker.marker_id == "people-green"
            && marker.kind
                == MarkerKind::Generic {
                    family: MarkerFamily::People,
                }
    }));
    assert_eq!(
        read_zip_entry(&fs::read(&path).unwrap(), "resources/unknown.bin"),
        b"\0\x01\x02future".to_vec()
    );

    let _ = fs::remove_file(path);
}

#[test]
fn marker_edit_is_atomic_when_the_markers_field_has_the_wrong_shape() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["children"]["attached"][1]["markers"] = json!({});
            Ok(())
        })
        .expect("make malformed markers");
    let before = package.content().clone();

    let error = package
        .apply_topic_markers(
            0,
            "topic-other",
            &TopicMarkerPatch {
                add: vec!["flag-blue".to_owned()],
                ..TopicMarkerPatch::default()
            },
        )
        .expect_err("malformed marker collection must fail");
    assert_eq!(error, TopicMarkerError::InvalidMarkersField { index: 0 });
    assert_eq!(package.content(), &before);
}

#[test]
fn typed_slots_reject_generic_additions_that_would_create_marker_conflicts() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let before = package.content().clone();
    let error = package
        .apply_topic_markers(
            0,
            "topic-markers",
            &TopicMarkerPatch {
                add: vec!["priority-4".to_owned()],
                ..TopicMarkerPatch::default()
            },
        )
        .expect_err("reserved marker must be rejected");
    assert_eq!(
        error,
        TopicMarkerError::ReservedMarkerId("priority-4".to_owned())
    );
    assert_eq!(package.content(), &before);
}

#[test]
fn todo_toggle_sets_completion_rendering_and_preserves_unknown_task_fields() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let before_other = find_topic(package.content(), "topic-other").clone();
    let initial = package
        .topic_todo(0, "topic-markers")
        .expect("read initial todo");
    assert_eq!(initial.status, TodoStatus::InProgress { eighths: 4 });
    assert_eq!(initial.progress, Some(TaskProgress::Half));
    assert_eq!(initial.rendering.text_decoration, TodoTextDecoration::None);
    assert_eq!(initial.rendering.tone, VisualTone::Normal);
    assert_eq!(initial.rendering.text_opacity, OpacitySemantics::Full);
    assert_eq!(initial.rendering.connector_opacity, OpacitySemantics::Full);

    let completed = package
        .apply_topic_todo(
            0,
            "topic-markers",
            &TopicTodoPatch {
                update: TodoUpdate::ToggleCompletion,
            },
        )
        .expect("complete todo");
    assert_eq!(completed.status, TodoStatus::Done);
    assert_eq!(completed.progress, Some(TaskProgress::Done));
    assert_eq!(
        completed.rendering.text_decoration,
        TodoTextDecoration::LineThrough
    );
    assert_eq!(completed.rendering.tone, VisualTone::Muted);
    assert_eq!(completed.rendering.text_opacity, OpacitySemantics::Reduced);
    assert_eq!(
        completed.rendering.connector_opacity,
        OpacitySemantics::Reduced
    );

    let topic = find_topic(package.content(), "topic-markers");
    assert_eq!(
        topic["markers"][2]["futureProgressField"],
        json!({"kept": "progress-marker"})
    );
    assert_eq!(topic["task"]["source"], json!("opaque"));
    assert_eq!(topic["task"]["unknown"], json!([1, 2]));
    assert_eq!(topic["futureTopicField"], json!({"kept": "topic"}));
    assert_eq!(find_topic(package.content(), "topic-other"), &before_other);

    let completed_path = test_path("todo-completed");
    package
        .save_as(&completed_path)
        .expect("save completed todo");
    let saved = XmindPackage::open(&completed_path).expect("reopen completed todo");
    let saved_todo = saved
        .topic_todo(0, "topic-markers")
        .expect("read reopened completed todo");
    assert_eq!(saved_todo.status, TodoStatus::Done);
    assert_eq!(
        saved_todo.rendering.text_decoration,
        TodoTextDecoration::LineThrough
    );
    assert_eq!(
        find_topic(saved.content(), "topic-markers")["task"]["unknown"],
        json!([1, 2])
    );
    let _ = fs::remove_file(completed_path);

    let restored = package
        .apply_topic_todo(
            0,
            "topic-markers",
            &TopicTodoPatch {
                update: TodoUpdate::ToggleCompletion,
            },
        )
        .expect("restore todo");
    assert_eq!(restored.status, TodoStatus::NotStarted);
    assert_eq!(restored.progress, Some(TaskProgress::Start));
    assert_eq!(restored.rendering.text_decoration, TodoTextDecoration::None);
}

#[test]
fn todo_without_a_progress_marker_becomes_done_without_touching_other_markers() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let before = find_topic(package.content(), "topic-no-markers").clone();
    assert_eq!(
        package
            .topic_todo(0, "topic-no-markers")
            .expect("read empty todo")
            .status,
        TodoStatus::NotStarted
    );

    let completed = package
        .apply_topic_todo(
            0,
            "topic-no-markers",
            &TopicTodoPatch {
                update: TodoUpdate::ToggleCompletion,
            },
        )
        .expect("complete empty todo");
    assert_eq!(completed.status, TodoStatus::Done);

    let topic = find_topic(package.content(), "topic-no-markers");
    assert_eq!(topic["markers"], json!([{ "markerId": "task-done" }]));
    assert_eq!(topic["title"], before["title"]);
    assert_eq!(topic["task"], before["task"]);
    assert_eq!(topic["children"], before["children"]);
}

#[test]
fn paused_progress_has_a_distinct_status_and_no_completion_style() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let state = package
        .apply_topic_todo(
            0,
            "topic-markers",
            &TopicTodoPatch {
                update: TodoUpdate::SetProgress(TaskProgress::Paused),
            },
        )
        .expect("pause todo");
    assert_eq!(state.status, TodoStatus::Paused);
    assert_eq!(state.progress, Some(TaskProgress::Paused));
    assert_eq!(state.rendering.tone, VisualTone::Normal);
    assert_eq!(state.rendering.text_decoration, TodoTextDecoration::None);
}

#[test]
fn capability_fixture_root_accepts_header_markers_without_changing_its_subtree() {
    let mut package = XmindPackage::open(&fixture_path(UI_FIXTURE)).expect("open UI fixture");
    let manifest = ui_fixture_manifest();
    let root_id = package.content()[0]["rootTopic"]["id"]
        .as_str()
        .expect("fixture root id")
        .to_owned();
    let root_before = package.content()[0]["rootTopic"].clone();
    let sheet_before = package.content()[0].clone();

    assert!(
        package
            .topic_header_markers(0, &root_id)
            .expect("read fixture header")
            .markers
            .is_empty()
    );
    assert_eq!(
        package
            .topic_todo(0, &root_id)
            .expect("read fixture todo")
            .status,
        TodoStatus::NotStarted
    );

    package
        .apply_topic_markers(
            0,
            &root_id,
            &TopicMarkerPatch {
                priority: SlotUpdate::Set(PriorityMarker::Priority2),
                task_progress: SlotUpdate::Set(TaskProgress::Quarter),
                ..TopicMarkerPatch::default()
            },
        )
        .expect("apply fixture markers");

    let root_after = &package.content()[0]["rootTopic"];
    assert_eq!(root_after["children"], root_before["children"]);
    for key in root_before.as_object().unwrap().keys() {
        assert_eq!(
            root_after[key], root_before[key],
            "root field {key} changed"
        );
    }
    for key in sheet_before.as_object().unwrap().keys() {
        if key != "rootTopic" {
            assert_eq!(
                package.content()[0][key],
                sheet_before[key],
                "sheet field {key} changed"
            );
        }
    }
    assert_eq!(
        count_topics(package.content()),
        manifest["topics"].as_u64().unwrap() as usize
    );

    let header = package
        .topic_header_markers(0, &root_id)
        .expect("read updated fixture header");
    assert_eq!(
        header
            .markers
            .iter()
            .map(|marker| marker.marker_id.as_str())
            .collect::<Vec<_>>(),
        vec!["priority-2", "task-quarter"]
    );
    assert_eq!(
        package
            .topic_todo(0, &root_id)
            .expect("read updated fixture todo")
            .status,
        TodoStatus::InProgress { eighths: 2 }
    );

    let path = test_path("fixture-markers");
    package.save_as(&path).expect("save fixture");
    let reopened = XmindPackage::open(&path).expect("reopen fixture");
    assert_eq!(
        reopened
            .topic_header_markers(0, &root_id)
            .expect("read reopened fixture header")
            .markers
            .len(),
        2
    );
    let _ = fs::remove_file(path);
}

fn synthetic_package() -> Vec<u8> {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "title": "Marker and TODO fixture",
            "futureSheetField": {"kept": true},
            "rootTopic": {
                "id": "root-1",
                "class": "topic",
                "title": "Root",
                "futureRootField": {"kept": true},
                "children": {
                    "attached": [
                        {
                            "id": "topic-markers",
                            "class": "topic",
                            "title": "Markers",
                            "markers": [
                                {
                                    "markerId": "priority-1",
                                    "futureMarkerField": {"kept": "priority-marker"}
                                },
                                {
                                    "markerId": "tag-red",
                                    "groupId": "tagMarkers",
                                    "futureTagField": [1, 2, 3]
                                },
                                {
                                    "markerId": "task-half",
                                    "futureProgressField": {"kept": "progress-marker"}
                                },
                                {
                                    "markerId": "star-yellow",
                                    "futureStarField": "kept"
                                },
                                {"futureOpaque": "kept"},
                                "unexpected-string-marker",
                                {"opaqueNumber": 42}
                            ],
                            "task": {
                                "source": "opaque",
                                "unknown": [1, 2],
                                "nested": {"kept": true}
                            },
                            "futureTopicField": {"kept": "topic"},
                            "children": {
                                "attached": [
                                    {
                                        "id": "topic-leaf",
                                        "class": "topic",
                                        "title": "Leaf"
                                    }
                                ]
                            }
                        },
                        {
                            "id": "topic-other",
                            "class": "topic",
                            "title": "Unrelated",
                            "markers": [
                                {"markerId": "priority-3", "opaque": "other"},
                                {"markerId": "flag-red"}
                            ],
                            "futureOtherField": "kept"
                        },
                        {
                            "id": "topic-no-markers",
                            "class": "topic",
                            "title": "No markers",
                            "task": {"status": "opaque", "future": true},
                            "children": {"attached": []}
                        }
                    ]
                }
            }
        }
    ]);
    zip_bytes(&[
        (
            "content.json",
            serde_json::to_vec(&content).unwrap().as_slice(),
        ),
        (
            "metadata.json",
            br#"{"activeSheetId":"sheet-1","futureMetadata":true}"#.as_slice(),
        ),
        ("resources/unknown.bin", b"\0\x01\x02future".as_slice()),
    ])
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("start synthetic ZIP entry");
            writer.write_all(bytes).expect("write synthetic ZIP entry");
        }
        writer.finish().expect("finish synthetic ZIP");
    }
    output.into_inner()
}

fn read_zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("read ZIP");
    let mut entry = archive.by_name(name).expect("find ZIP entry");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read ZIP payload");
    bytes
}

fn find_topic<'a>(value: &'a Value, id: &str) -> &'a Value {
    if value.get("id").and_then(Value::as_str) == Some(id) {
        return value;
    }
    match value {
        Value::Object(object) => object
            .values()
            .find_map(|value| find_topic_optional(value, id))
            .unwrap_or_else(|| panic!("topic {id} not found")),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_topic_optional(value, id))
            .unwrap_or_else(|| panic!("topic {id} not found")),
        _ => panic!("topic {id} not found"),
    }
}

fn find_topic_optional<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    if value.get("id").and_then(Value::as_str) == Some(id) {
        return Some(value);
    }
    match value {
        Value::Object(object) => object
            .values()
            .find_map(|value| find_topic_optional(value, id)),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_topic_optional(value, id)),
        _ => None,
    }
}

fn count_topics(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.get("class").and_then(Value::as_str) == Some("topic"))
                + object.values().map(count_topics).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(count_topics).sum(),
        _ => 0,
    }
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn ui_fixture_manifest() -> Value {
    let bytes = fs::read(fixture_path(UI_FIXTURE_MANIFEST)).expect("read UI fixture manifest");
    serde_json::from_slice(&bytes).expect("parse UI fixture manifest")
}

fn test_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kite-mindmap-markers-{label}-{}-{}.xmind",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}

#[allow(dead_code)]
fn assert_todo(state: &TopicTodoState) {
    let _ = state;
}
