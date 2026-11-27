use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kite_core::mindmap::{
    ConnectorColor, ConnectorCornerRadius, ConnectorKind, ConnectorLinePattern, ConnectorStyle,
    ConnectorStylePatch, ConnectorWidth, DocumentGraphStylePatch, GraphStructure, GraphStyleError,
    XmindPackage,
};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const UI_FIXTURE: &str = "tests/fixtures/mindmap/kite-rust-gpui-capability.xmind";
static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn supported_graph_and_connector_values_have_stable_xmind_mappings() {
    let structures = [
        (GraphStructure::MindMap, "org.xmind.ui.map"),
        (GraphStructure::LogicRight, "org.xmind.ui.logic.right"),
        (GraphStructure::LogicLeft, "org.xmind.ui.logic.left"),
        (GraphStructure::OrgChart, "org.xmind.ui.org-chart.down"),
        (GraphStructure::Tree, "org.xmind.ui.tree.right"),
    ];
    for (structure, serialized) in structures {
        assert_eq!(structure.xmind_value(), serialized);
        assert_eq!(
            serde_json::to_string(&structure).unwrap(),
            format!("\"{serialized}\"")
        );
        assert_eq!(
            serde_json::from_str::<GraphStructure>(&format!("\"{serialized}\"")).unwrap(),
            structure
        );
    }
    assert_eq!(
        GraphStructure::from_xmind_value("org.xmind.ui.map.clockwise"),
        GraphStructure::MindMap
    );
    assert_eq!(
        GraphStructure::from_xmind_value("org.xmind.ui.logic-chart.right"),
        GraphStructure::LogicRight
    );

    let connectors = [
        (ConnectorKind::Curved, "org.xmind.branchConnection.curve"),
        (
            ConnectorKind::Straight,
            "org.xmind.branchConnection.straight",
        ),
        (ConnectorKind::Elbow, "org.xmind.branchConnection.elbow"),
        (
            ConnectorKind::RoundedElbow,
            "org.xmind.branchConnection.roundedElbow",
        ),
    ];
    for (connector, serialized) in connectors {
        assert_eq!(connector.xmind_value(), serialized);
        assert_eq!(
            serde_json::to_string(&connector).unwrap(),
            format!("\"{serialized}\"")
        );
        assert_eq!(
            serde_json::from_str::<ConnectorKind>(&format!("\"{serialized}\"")).unwrap(),
            connector
        );
    }
}

#[test]
fn ui_fixture_exposes_logic_right_without_becoming_dirty() {
    let package = XmindPackage::open(&fixture_path(UI_FIXTURE)).expect("open UI fixture");
    let style = package
        .document_graph_style(0)
        .expect("read document graph style");

    assert_eq!(style.structure, Some(GraphStructure::LogicRight));
    assert_eq!(style.connector, None);
    assert!(!package.is_dirty());
}

#[test]
fn connector_read_uses_root_fields_before_theme_field_fallbacks() {
    let package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let connector = package
        .document_graph_style(0)
        .expect("read layered connector")
        .connector
        .expect("root and theme connector");

    assert_eq!(connector.kind, ConnectorKind::Curved);
    assert_eq!(connector.width, Some(ConnectorWidth::new("1pt").unwrap()));
    assert_eq!(
        connector.color,
        Some(ConnectorColor::new("#AABBCC").unwrap()),
        "color is inherited from the theme when root local style omits it"
    );
}

#[test]
fn connector_reads_theme_central_topic_when_root_style_is_absent() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]
                .as_object_mut()
                .expect("root topic object")
                .remove("style");
            Ok(())
        })
        .expect("remove root style");

    let connector = package
        .document_graph_style(0)
        .expect("read fallback")
        .connector
        .expect("theme fallback connector");
    assert_eq!(connector.kind, ConnectorKind::Straight);
    assert_eq!(
        connector.color,
        Some(ConnectorColor::new("#AABBCC").unwrap())
    );
}

#[test]
fn ui_fixture_style_edit_preserves_unrelated_topic_styles() {
    let mut package = XmindPackage::open(&fixture_path(UI_FIXTURE)).expect("open UI fixture");
    let styled_topic = first_styled_topic(package.content()).expect("fixture has styled topics");
    let topic_id = styled_topic["id"]
        .as_str()
        .expect("styled topic id")
        .to_owned();
    let original_style = styled_topic["style"].clone();

    let patch = DocumentGraphStylePatch {
        structure: Some(GraphStructure::LogicLeft),
        connector: Some(ConnectorStylePatch {
            kind: Some(ConnectorKind::RoundedElbow),
            corner_radius: Some(ConnectorCornerRadius::new(12).unwrap()),
            width: Some(ConnectorWidth::points(2)),
            color: Some(ConnectorColor::new("#112233").unwrap()),
            pattern: Some(ConnectorLinePattern::Solid),
        }),
    };
    package
        .apply_document_graph_style(0, &patch)
        .expect("apply graph style");

    assert!(package.is_dirty());
    assert_eq!(
        package.content()[0]["rootTopic"]["structureClass"],
        json!("org.xmind.ui.logic.left")
    );
    assert_eq!(
        package.content()[0]["rootTopic"]["style"]["properties"]["line-class"],
        json!("org.xmind.branchConnection.roundedElbow")
    );
    assert_eq!(
        package.content()[0]["rootTopic"]["style"]["properties"]["line-corner"],
        json!("12")
    );
    assert_eq!(
        find_topic_by_id(package.content(), &topic_id).unwrap()["style"],
        original_style,
        "editing the document style must not rewrite topic-local styles"
    );

    let output = test_path("graph-style-ui-fixture");
    package.save_as(&output).expect("save styled fixture");
    let reopened = XmindPackage::open(&output).expect("reopen styled fixture");
    assert_eq!(
        reopened.document_graph_style(0).unwrap(),
        kite_core::mindmap::DocumentGraphStyle {
            structure: Some(GraphStructure::LogicLeft),
            connector: Some(ConnectorStyle {
                kind: ConnectorKind::RoundedElbow,
                corner_radius: Some(ConnectorCornerRadius::new(12).unwrap()),
                width: Some(ConnectorWidth::points(2)),
                color: Some(ConnectorColor::new("#112233").unwrap()),
                pattern: Some(ConnectorLinePattern::Solid),
            }),
        }
    );
    assert_eq!(
        find_topic_by_id(reopened.content(), &topic_id).unwrap()["style"],
        original_style
    );

    let _ = fs::remove_file(output);
}

#[test]
fn synthetic_theme_edit_preserves_unknown_json_and_unrelated_styles() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let patch = DocumentGraphStylePatch {
        structure: Some(GraphStructure::OrgChart),
        connector: Some(ConnectorStylePatch {
            kind: Some(ConnectorKind::Straight),
            corner_radius: Some(ConnectorCornerRadius::new(4).unwrap()),
            width: Some(ConnectorWidth::new("3pt").unwrap()),
            color: Some(ConnectorColor::new("#00000066").unwrap()),
            pattern: Some(ConnectorLinePattern::Dash),
        }),
    };
    package
        .apply_document_graph_style(0, &patch)
        .expect("apply synthetic style");

    let content = package.content();
    assert_eq!(
        content[0]["rootTopic"]["structureClass"],
        json!("org.xmind.ui.org-chart.down")
    );
    assert_eq!(content[0]["futureSheetField"]["kept"], json!(true));
    assert_eq!(
        content[0]["rootTopic"]["futureRootField"],
        json!({"kept": true})
    );
    assert_eq!(
        content[0]["rootTopic"]["style"]["futureStyleField"],
        json!(["kept"])
    );
    assert_eq!(
        content[0]["rootTopic"]["style"]["properties"]["unknown-property"],
        json!("kept")
    );
    assert_eq!(
        content[0]["theme"]["centralTopic"]["properties"]["unknown-central-property"],
        json!("kept")
    );
    for category in ["centralTopic", "mainTopic", "subTopic", "level3"] {
        assert_eq!(
            content[0]["theme"][category]["properties"]["line-class"],
            json!("org.xmind.branchConnection.straight"),
            "document connector patch must be synchronized to theme.{category}"
        );
        assert_eq!(
            content[0]["theme"][category]["properties"]["line-width"],
            json!("3pt")
        );
        assert_eq!(
            content[0]["theme"][category]["properties"]["line-color"],
            json!("#00000066")
        );
        assert_eq!(
            content[0]["theme"][category]["properties"]["line-pattern"],
            json!("dash")
        );
        assert_eq!(
            content[0]["theme"][category]["properties"]["line-corner"],
            json!("4")
        );
    }
    assert_eq!(
        content[0]["theme"]["mainTopic"]["properties"]["unknown-main-property"],
        json!("kept")
    );
    assert_eq!(
        content[0]["theme"]["subTopic"]["properties"]["unknown-sub-property"],
        json!("kept")
    );
    assert_eq!(
        content[0]["theme"]["level3"]["properties"]["unknown-level-property"],
        json!("kept")
    );
    assert_eq!(
        content[0]["theme"]["map"],
        json!({"properties": {"svg:fill": "#FFFFFF"}})
    );
    assert_eq!(content[0]["theme"]["futureThemeField"], json!("kept"));
    assert_eq!(
        content[0]["extensions"][0]["content"]["centralTopic"],
        json!("org.xmind.ui.map.clockwise"),
        "skeleton extension semantics are preserved until separately audited"
    );
    assert_eq!(
        content[0]["rootTopic"]["children"]["attached"][0]["style"],
        json!({
            "id": "child-style",
            "properties": {"fo:font-size": "14pt", "future-child-property": "kept"},
            "futureChildStyleField": 7
        })
    );

    let output = test_path("synthetic-theme");
    package.save_as(&output).expect("save synthetic package");
    assert_eq!(
        read_zip_entry(&fs::read(&output).unwrap(), "metadata.json"),
        br#"{"activeSheetId":"sheet-1","futureMetadata":true}"#.to_vec()
    );
    let reopened = XmindPackage::open(&output).expect("reopen synthetic package");
    for category in ["centralTopic", "mainTopic", "subTopic", "level3"] {
        assert_eq!(
            reopened.content()[0]["theme"][category]["properties"]["line-pattern"],
            json!("dash")
        );
        assert_eq!(
            reopened.content()[0]["theme"][category]["properties"]["line-color"],
            json!("#00000066")
        );
    }
    assert_eq!(
        reopened.content()[0]["theme"]["subTopic"]["properties"]["unknown-sub-property"],
        json!("kept")
    );
    assert_eq!(
        reopened.content()[0]["theme"]["level3"]["properties"]["unknown-level-property"],
        json!("kept")
    );

    let _ = fs::remove_file(output);
}

#[test]
fn theme_edit_creates_central_topic_but_does_not_invent_other_categories() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            let theme = content[0]["theme"].as_object_mut().expect("theme object");
            theme.remove("centralTopic");
            theme.remove("subTopic");
            theme.remove("level3");
            Ok(())
        })
        .expect("remove optional theme categories");

    package
        .apply_document_graph_style(
            0,
            &DocumentGraphStylePatch {
                structure: None,
                connector: Some(ConnectorStylePatch {
                    color: Some(ConnectorColor::new("#123456").unwrap()),
                    ..ConnectorStylePatch::default()
                }),
            },
        )
        .expect("apply color to surviving theme categories");

    let theme = &package.content()[0]["theme"];
    assert_eq!(
        theme["centralTopic"]["properties"]["line-color"],
        json!("#123456")
    );
    assert_eq!(
        theme["mainTopic"]["properties"]["line-color"],
        json!("#123456")
    );
    assert!(theme.get("subTopic").is_none());
    assert!(theme.get("level3").is_none());
    assert_eq!(theme["futureThemeField"], json!("kept"));
}

#[test]
fn graph_edit_does_not_create_a_theme_when_the_sheet_has_none() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            content[0]
                .as_object_mut()
                .expect("sheet object")
                .remove("theme");
            Ok(())
        })
        .expect("remove theme");

    package
        .apply_document_graph_style(
            0,
            &DocumentGraphStylePatch {
                structure: None,
                connector: Some(ConnectorStylePatch {
                    color: Some(ConnectorColor::new("#ABC123").unwrap()),
                    ..ConnectorStylePatch::default()
                }),
            },
        )
        .expect("apply root-local color");

    assert!(package.content()[0].get("theme").is_none());
    assert_eq!(
        package.content()[0]["rootTopic"]["style"]["properties"]["line-color"],
        json!("#ABC123")
    );
}

#[test]
fn unknown_connector_and_unknown_properties_survive_targeted_edits() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["style"]["properties"]["line-class"] =
                json!("org.xmind.branchConnection.future");
            content[0]["rootTopic"]["style"]["properties"]["line-width"] = json!("7pt");
            Ok(())
        })
        .expect("seed unknown connector");

    package
        .apply_document_graph_style(
            0,
            &DocumentGraphStylePatch {
                structure: Some(GraphStructure::Tree),
                connector: None,
            },
        )
        .expect("apply structure only");

    let style = package.document_graph_style(0).unwrap();
    assert_eq!(
        style.connector.as_ref().unwrap().kind,
        ConnectorKind::Other("org.xmind.branchConnection.future".to_owned())
    );
    assert_eq!(
        style.connector.as_ref().unwrap().width,
        Some(ConnectorWidth::new("7pt").unwrap())
    );

    package
        .apply_document_graph_style(
            0,
            &DocumentGraphStylePatch {
                structure: None,
                connector: Some(ConnectorStylePatch {
                    kind: None,
                    corner_radius: None,
                    width: None,
                    color: Some(ConnectorColor::new("#ABCDEF").unwrap()),
                    pattern: None,
                }),
            },
        )
        .expect("apply color only");
    let updated = package.document_graph_style(0).unwrap().connector.unwrap();
    assert_eq!(
        updated.kind,
        ConnectorKind::Other("org.xmind.branchConnection.future".to_owned())
    );
    assert_eq!(updated.width, Some(ConnectorWidth::new("7pt").unwrap()));
    assert_eq!(updated.color, Some(ConnectorColor::new("#ABCDEF").unwrap()));
}

#[test]
fn invalid_target_does_not_partially_mutate_content() {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "future": {"kept": true}
        }
    ]);
    let mut package = XmindPackage::from_bytes(zip_bytes(&[(
        "content.json",
        serde_json::to_vec(&content).unwrap().as_slice(),
    )]))
    .expect("parse package without root topic");
    let before = package.content().clone();

    let error = package
        .apply_document_graph_style(0, &DocumentGraphStylePatch::default())
        .expect_err("default patch still needs valid target structures");
    assert!(matches!(
        error,
        GraphStyleError::MissingRootTopic { index: 0 }
    ));
    assert_eq!(package.content(), &before);
    assert!(!package.is_dirty());

    let error = package
        .apply_document_graph_style(1, &DocumentGraphStylePatch::default())
        .expect_err("invalid sheet index must fail");
    assert!(matches!(error, GraphStyleError::SheetNotFound { index: 1 }));
    assert_eq!(package.content(), &before);
}

fn synthetic_package() -> Vec<u8> {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "title": "Graph style fixture",
            "futureSheetField": {"kept": true},
            "rootTopic": {
                "id": "root-1",
                "class": "topic",
                "title": "Root",
                "structureClass": "org.xmind.ui.map.clockwise",
                "futureRootField": {"kept": true},
                "style": {
                    "id": "root-style",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.curve",
                        "line-width": "1pt",
                        "unknown-property": "kept"
                    },
                    "futureStyleField": ["kept"]
                },
                "children": {
                    "attached": [
                        {
                            "id": "child-1",
                            "class": "topic",
                            "title": "Child",
                            "style": {
                                "id": "child-style",
                                "properties": {
                                    "fo:font-size": "14pt",
                                    "future-child-property": "kept"
                                },
                                "futureChildStyleField": 7
                            }
                        }
                    ]
                }
            },
            "theme": {
                "centralTopic": {
                    "id": "central-style",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.straight",
                        "line-color": "#AABBCC",
                        "unknown-central-property": "kept"
                    },
                    "futureThemeStyle": true
                },
                "mainTopic": {
                    "id": "main-style",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.elbow",
                        "unknown-main-property": "kept"
                    }
                },
                "subTopic": {
                    "id": "sub-style",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.curve",
                        "unknown-sub-property": "kept"
                    }
                },
                "level3": {
                    "id": "level3-style",
                    "properties": {
                        "line-class": "org.xmind.branchConnection.curve",
                        "unknown-level-property": "kept"
                    }
                },
                "map": {
                    "properties": {"svg:fill": "#FFFFFF"}
                },
                "futureThemeField": "kept"
            },
            "extensions": [
                {
                    "provider": "org.xmind.ui.skeleton.structure.style",
                    "content": {
                        "centralTopic": "org.xmind.ui.map.clockwise",
                        "mainTopic": "org.xmind.ui.logic.right"
                    },
                    "futureExtensionField": 11
                }
            ]
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

fn first_styled_topic(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(object) => {
            if object.get("class").and_then(Value::as_str) == Some("topic")
                && object.get("style").is_some_and(Value::is_object)
            {
                return Some(value);
            }
            object.values().find_map(first_styled_topic)
        }
        Value::Array(values) => values.iter().find_map(first_styled_topic),
        _ => None,
    }
}

fn find_topic_by_id<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    match value {
        Value::Object(object) => {
            if object.get("id").and_then(Value::as_str) == Some(id) {
                return Some(value);
            }
            object
                .values()
                .find_map(|value| find_topic_by_id(value, id))
        }
        Value::Array(values) => values.iter().find_map(|value| find_topic_by_id(value, id)),
        _ => None,
    }
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn test_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kite-mindmap-graph-{label}-{}-{}.xmind",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}
