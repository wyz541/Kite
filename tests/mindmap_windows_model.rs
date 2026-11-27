use std::collections::BTreeSet;
use std::path::Path;

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_ffi::MindmapGraphStructureDto;
use kite_core::mindmap_ui_model::MindmapTree;

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
