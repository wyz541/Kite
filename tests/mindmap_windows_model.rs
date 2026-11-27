use std::collections::BTreeSet;
use std::path::Path;

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_ffi::{MindmapDatePartOrderDto, MindmapGraphStructureDto};
use kite_core::mindmap_ui_model::{
    MindmapTree, letter_spacing_preview, missing_font_family, parse_system_date_pattern,
    resolve_render_font,
};

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
fn modern_task_fields_drive_completion_without_legacy_markers() {
    let content = r#"[{
      "id": "sheet",
      "rootTopic": {
        "id": "root",
        "title": "Root",
        "children": {"attached": [{
          "id": "done",
          "title": "Done with 27.1 fields",
          "taskPresentation": "todo-checkbox",
          "todoStatus": "done",
          "todoProgress": 1.0,
          "futureTaskField": {"kept": true}
        }]}
      }
    }]"#;
    let tree = MindmapTree::from_content_json(content).expect("parse modern task tree");
    let done = &tree.topics["done"];
    assert!(done.is_complete);
    assert_eq!(done.task_presentation.as_deref(), Some("todo-checkbox"));
    assert_eq!(done.todo_status.as_deref(), Some("done"));
    assert_eq!(done.todo_progress, Some(1.0));
    assert!(done.marker_summary().contains('✓'));
}

#[test]
fn edge_anchors_follow_left_right_and_org_chart_directions() {
    let content = r#"[{
      "id": "sheet",
      "rootTopic": {
        "id": "root",
        "title": "Root",
        "children": {"attached": [
          {"id": "left", "title": "Left"},
          {"id": "right", "title": "Right"}
        ]}
      }
    }]"#;
    let tree = MindmapTree::from_content_json(content).expect("parse anchor tree");
    let collapsed = BTreeSet::new();

    let left_layout = tree.layout(&collapsed, MindmapGraphStructureDto::LogicLeft, 1.0);
    let left = left_layout
        .edge_anchors("root", "left", MindmapGraphStructureDto::LogicLeft)
        .expect("logic-left anchors");
    let parent = left_layout.node("root").expect("parent layout");
    let child = left_layout.node("left").expect("child layout");
    assert_eq!(left.start_x, parent.center_x - parent.width / 2.0);
    assert_eq!(left.end_x, child.center_x + child.width / 2.0);

    let org_layout = tree.layout(&collapsed, MindmapGraphStructureDto::OrgChart, 1.0);
    let org = org_layout
        .edge_anchors("root", "right", MindmapGraphStructureDto::OrgChart)
        .expect("org anchors");
    assert!(org.start_y < org.end_y, "org connector starts below parent");
    assert_eq!(org.start_x, org_layout.node("root").unwrap().center_x);
}

#[test]
fn marker_header_is_bounded_and_reports_overflow() {
    let markers = (0..11)
        .map(|index| format!(r#"{{"markerId":"star-{index}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let content = format!(
        r#"[{{"id":"sheet","rootTopic":{{"id":"root","title":"Root","children":{{"attached":[{{"id":"topic","title":"Topic","markers":[{markers}]}}]}}}}}}]"#
    );
    let tree = MindmapTree::from_content_json(&content).expect("parse marker tree");
    let summary = tree.topics["topic"].marker_summary();
    assert!(summary.ends_with("+3"));
    assert_eq!(summary.matches('★').count(), 8);
}

#[test]
fn locale_font_and_spacing_helpers_are_deterministic() {
    assert_eq!(
        parse_system_date_pattern("M/d/yyyy", "/"),
        (MindmapDatePartOrderDto::MonthDayYear, "/".to_string())
    );
    assert_eq!(
        parse_system_date_pattern("dd.MM.yyyy", "."),
        (MindmapDatePartOrderDto::DayMonthYear, ".".to_string())
    );
    assert_eq!(
        parse_system_date_pattern("yyyy-MM-dd", "-"),
        (MindmapDatePartOrderDto::YearMonthDay, "-".to_string())
    );

    let fonts = vec![
        "Segoe UI".to_string(),
        "Arial".to_string(),
        "Consolas".to_string(),
    ];
    assert_eq!(missing_font_family(Some("Arial"), &fonts), None);
    assert_eq!(
        missing_font_family(Some("Missing Sans"), &fonts),
        Some("Missing Sans".to_string())
    );
    assert_eq!(
        resolve_render_font(Some("Missing Sans"), &fonts, "Segoe UI"),
        "Segoe UI"
    );
    assert_eq!(
        letter_spacing_preview("ABC", Some(0.2)),
        "A\u{2009}B\u{2009}C"
    );
    assert_eq!(letter_spacing_preview("ABC", None), "ABC");
}
