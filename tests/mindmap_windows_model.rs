use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_ffi::MindmapGraphStructureDto;
use kite_core::mindmap_ui_model::MindmapTree;

const UI_TREE_PARITY_DIR: &str = "tests/fixtures/mindmap/ui-tree-parity";

#[test]
fn capability_fixture_parses_and_lays_out_every_topic() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mindmap/kite-rust-gpui-capability.xmind");
    let package = XmindPackage::open(&path).expect("open capability fixture");
    let content = serde_json::to_string(package.content()).expect("serialize content.json");
    let tree = MindmapTree::from_content_json(&content).expect("parse capability tree");

    assert_eq!(tree.topics.len(), 3_019);
    assert!(tree.root_id.is_some());

    let collapsed = BTreeSet::new();
    for structure in [
        MindmapGraphStructureDto::MindMap,
        MindmapGraphStructureDto::LogicRight,
        MindmapGraphStructureDto::LogicLeft,
        MindmapGraphStructureDto::OrgChart,
        MindmapGraphStructureDto::Tree,
    ] {
        let layout = tree.layout(&collapsed, structure, 0.82);
        assert_eq!(layout.nodes.len(), tree.topics.len());
        assert!(layout.width.is_finite() && layout.height.is_finite());
    }
}

#[test]
fn copied_topic_json_retains_unknown_fields() {
    let content = r#"[{
      "id": "sheet",
      "rootTopic": {
        "id": "root",
        "title": "Root",
        "unknownRoot": {"kept": true},
        "children": {"attached": [{
          "id": "topic",
          "title": "Topic",
          "unknownTopicField": {"kept": 42},
          "markers": [{"markerId": "priority-2", "opaque": "kept"}]
        }]}
      }
    }]"#;
    let tree = MindmapTree::from_content_json(content).expect("parse tree");
    assert_eq!(tree.topics["topic"].markers[0].label, "P2");
    let copied = kite_core::mindmap_ui_model::topic_json(content, "topic").expect("copy topic");
    assert!(copied.contains("unknownTopicField"));
    assert!(copied.contains("opaque"));
}

#[test]
fn windows_ui_tree_matches_the_shared_macos_canonical_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(UI_TREE_PARITY_DIR);
    let content = fs::read_to_string(root.join("content.json")).expect("read UI tree fixture");
    let tree = MindmapTree::from_content_json(&content).expect("parse UI tree fixture");
    let actual = tree.canonical_projection();
    let golden_path = root.join("expected.canonical.json");
    let hash_path = root.join("expected.canonical.sha256");

    if std::env::var_os("UPDATE_MINDMAP_UI_TREE_GOLDEN").is_some() {
        let mut pretty = serde_json::to_vec_pretty(&actual).expect("serialize canonical golden");
        pretty.push(b'\n');
        fs::write(&golden_path, pretty).expect("write canonical golden");
        fs::write(
            &hash_path,
            format!(
                "{}\n",
                tree.canonical_sha256().expect("hash canonical tree")
            ),
        )
        .expect("write canonical hash");
    }

    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(&golden_path).expect("read canonical UI tree golden"))
            .expect("parse canonical UI tree golden");
    assert_eq!(
        actual, expected,
        "Windows UI parser drifted from the shared golden"
    );
    let expected_hash = fs::read_to_string(hash_path)
        .expect("read canonical UI tree hash")
        .trim()
        .to_owned();
    assert_eq!(
        tree.canonical_sha256().expect("hash canonical tree"),
        expected_hash,
        "canonical compact JSON hash drifted"
    );

    assert_eq!(tree.structure, "logic-right");
    assert_eq!(tree.topics["empty-title"].title, "未命名主题");
    assert_eq!(tree.topics["alpha"].opaque_marker_count, 2);
    assert_eq!(
        tree.topics["alpha"]
            .markers
            .iter()
            .map(|marker| marker.marker_id.as_str())
            .collect::<Vec<_>>(),
        vec!["priority-1", "flag-red", "future-marker"]
    );

    let task = tree.topics["modern-task"]
        .task
        .as_ref()
        .expect("modern task semantics");
    assert_eq!(task.effective_status.as_deref(), Some("done"));
    assert_eq!(task.effective_progress, Some(1.0));
    assert_eq!(task.legacy_status.as_deref(), Some("not-started"));
    assert!(task.has_conflict);
    assert_eq!(task.task_start_date.as_deref(), Some("2026-11-27"));
    assert_eq!(task.task_due_date.as_deref(), Some("2026-11-30"));

    let string_progress = tree.topics["task-string-progress"]
        .task
        .as_ref()
        .expect("string task progress semantics");
    assert_eq!(string_progress.effective_status.as_deref(), Some("paused"));
    assert_eq!(string_progress.effective_progress, Some(0.5));
    assert_eq!(string_progress.todo_progress, Some(0.5));
}

#[test]
fn windows_ui_tree_parse_errors_match_the_shared_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(UI_TREE_PARITY_DIR);
    let expected: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("expected-errors.json")).expect("read expected errors"),
    )
    .expect("parse expected errors");
    let expected = expected.as_object().expect("expected errors object");

    for (fixture, expected_code) in expected {
        let content = fs::read_to_string(root.join("invalid").join(fixture))
            .unwrap_or_else(|error| panic!("read {fixture}: {error}"));
        let error = MindmapTree::from_content_json(&content)
            .expect_err("invalid UI tree should fail consistently");
        let expected_code = expected_code.as_str().expect("error code string");
        assert!(
            error.starts_with(expected_code),
            "{fixture}: expected {expected_code}, got {error}"
        );
    }
}
