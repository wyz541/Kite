use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kite_core::mindmap::{CONTENT_JSON_ENTRY, MAX_XMIND_PACKAGE_BYTES, XmindError, XmindPackage};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const UI_FIXTURE: &str = "tests/fixtures/mindmap/kite-rust-gpui-capability.xmind";
const UI_FIXTURE_MANIFEST: &str = "tests/fixtures/mindmap/kite-rust-gpui-capability.manifest.json";

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn ui_fixture_reports_expected_xmind_capability_shape() {
    let path = fixture_path(UI_FIXTURE);
    let package = XmindPackage::open(&path).expect("open UI fixture");
    let manifest = ui_fixture_manifest();
    let summary = package.summary();

    assert_eq!(summary.sheet_count, manifest_number(&manifest, "sheets"));
    assert_eq!(summary.entry_count, manifest_number(&manifest, "entries"));
    assert_eq!(summary.source_sha256, manifest_string(&manifest, "sha256"));
    assert_eq!(
        count_value(package.content(), "topic"),
        manifest_number(&manifest, "topics")
    );
    assert_eq!(
        count_styled_topics(package.content()),
        manifest_number(&manifest, "styledTopics")
    );
    assert_eq!(
        count_shape_classes(package.content()),
        manifest_number(&manifest, "shapeClassProperties")
    );
}

#[test]
fn unmodified_fixture_round_trip_is_byte_identical() {
    let original = fs::read(fixture_path(UI_FIXTURE)).expect("read UI fixture");
    let mut package = XmindPackage::from_bytes(original.clone()).expect("parse UI fixture");
    let output = test_path("ui-unmodified");

    let report = package.save_as(&output).expect("save unchanged fixture");
    assert!(report.byte_identical);
    assert!(report.rewritten_entries.is_empty());
    assert_eq!(fs::read(&output).expect("read saved fixture"), original);

    let _ = fs::remove_file(output);
}

#[test]
fn editing_one_topic_preserves_unknown_entries_and_all_other_capabilities() {
    let mut package = XmindPackage::open(&fixture_path(UI_FIXTURE)).expect("open UI fixture");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["title"] = Value::String("Kite UI fixture edited".into());
            Ok(())
        })
        .expect("edit root topic");

    let output = test_path("ui-edited");
    let report = package.save_as(&output).expect("save edited fixture");
    assert!(!report.byte_identical);
    assert_eq!(
        report.rewritten_entries,
        vec![CONTENT_JSON_ENTRY.to_string()]
    );

    let manifest = ui_fixture_manifest();
    let saved = XmindPackage::open(&output).expect("reopen edited fixture");
    assert_eq!(
        saved.summary().sheet_count,
        manifest_number(&manifest, "sheets")
    );
    assert_eq!(
        saved.content()[0]["rootTopic"]["title"],
        Value::String("Kite UI fixture edited".into())
    );
    assert_eq!(
        count_value(saved.content(), "topic"),
        manifest_number(&manifest, "topics")
    );
    assert_eq!(
        count_styled_topics(saved.content()),
        manifest_number(&manifest, "styledTopics")
    );
    assert_eq!(
        count_shape_classes(saved.content()),
        manifest_number(&manifest, "shapeClassProperties")
    );

    let original_entries = read_zip_entries(&fs::read(fixture_path(UI_FIXTURE)).unwrap());
    let saved_entries = read_zip_entries(&fs::read(&output).unwrap());
    assert_eq!(
        original_entries.get("metadata.json"),
        saved_entries.get("metadata.json")
    );
    assert_eq!(
        original_entries.get("manifest.json"),
        saved_entries.get("manifest.json")
    );

    let _ = fs::remove_file(output);
}

#[test]
fn unknown_json_fields_and_unknown_zip_entries_survive_edit() {
    let original = synthetic_package();
    let mut package = XmindPackage::from_bytes(original.clone()).expect("parse synthetic package");
    package
        .edit_content_json(|content| {
            content[0]["rootTopic"]["title"] = json!("Changed");
            Ok(())
        })
        .expect("edit synthetic package");

    let output = test_path("synthetic-edited");
    package.save_as(&output).expect("save synthetic package");
    let saved_bytes = fs::read(&output).expect("read synthetic package");
    let saved = XmindPackage::open(&output).expect("reopen synthetic package");
    let content = saved.content();

    assert_eq!(content[0]["rootTopic"]["title"], json!("Changed"));
    assert_eq!(content[0]["futureSheetField"]["kept"], json!(true));
    assert_eq!(
        content[0]["rootTopic"]["futureTopicField"]["nested"],
        json!(["a", "b"])
    );
    assert_eq!(
        read_zip_entry(&saved_bytes, "resources/unknown.bin"),
        b"\0\x01\x02future".to_vec()
    );

    let _ = fs::remove_file(output);
}

#[test]
fn save_rejects_external_modification() {
    let path = test_path("external-change");
    fs::write(&path, synthetic_package()).expect("write synthetic package");
    let mut package = XmindPackage::open(&path).expect("open synthetic package");
    fs::write(&path, b"changed outside Kite").expect("change file externally");

    let error = package.save().expect_err("external change must fail");
    assert!(matches!(error, XmindError::ExternalModification));

    let _ = fs::remove_file(path);
}

#[test]
fn legacy_content_xml_is_reported_explicitly() {
    let bytes = zip_bytes(&[("content.xml", b"<xmap-content/>".as_slice())]);
    let error = XmindPackage::from_bytes(bytes).expect_err("legacy package must be rejected");
    assert!(matches!(error, XmindError::LegacyContentXml));
}

#[test]
fn oversized_package_is_rejected_before_zip_parsing() {
    let error = XmindPackage::from_bytes(vec![0; MAX_XMIND_PACKAGE_BYTES as usize + 1])
        .expect_err("oversized package must be rejected");
    assert!(matches!(error, XmindError::PackageTooLarge));
}

#[test]
fn empty_or_non_array_content_json_is_rejected() {
    let empty = zip_bytes(&[("content.json", b"[]".as_slice())]);
    let error = XmindPackage::from_bytes(empty).expect_err("empty content must be rejected");
    assert!(matches!(error, XmindError::EmptyContent));

    let object = zip_bytes(&[("content.json", b"{}".as_slice())]);
    let error = XmindPackage::from_bytes(object).expect_err("object content must be rejected");
    assert!(matches!(error, XmindError::InvalidContentShape));
}

fn synthetic_package() -> Vec<u8> {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "title": "Capability fixture",
            "futureSheetField": { "kept": true },
            "rootTopic": {
                "id": "root-1",
                "class": "topic",
                "title": "Original",
                "children": {
                    "attached": [
                        {
                            "id": "child-1",
                            "class": "topic",
                            "title": "Styled child",
                            "style": {
                                "id": "style-1",
                                "properties": {
                                    "fo:font-size": "14pt",
                                    "svg:fill": "#112233"
                                }
                            },
                            "shape-class": "rounded-rectangle"
                        }
                    ]
                },
                "futureTopicField": { "nested": ["a", "b"] }
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

fn read_zip_entries(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("read ZIP");
    let mut entries = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read ZIP entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read ZIP payload");
        entries.insert(name, bytes);
    }
    entries
}

fn read_zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("read ZIP");
    let mut entry = archive.by_name(name).expect("find ZIP entry");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read ZIP payload");
    bytes
}

fn count_value(value: &Value, key: &str) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.get("class").and_then(Value::as_str) == Some(key))
                + object
                    .values()
                    .map(|value| count_value(value, key))
                    .sum::<usize>()
        }
        Value::Array(values) => values.iter().map(|value| count_value(value, key)).sum(),
        _ => 0,
    }
}

fn count_styled_topics(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(
                object.get("class").and_then(Value::as_str) == Some("topic")
                    && object.contains_key("style"),
            ) + object.values().map(count_styled_topics).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(count_styled_topics).sum(),
        _ => 0,
    }
}

fn count_shape_classes(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.contains_key("shape-class"))
                + object.values().map(count_shape_classes).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(count_shape_classes).sum(),
        _ => 0,
    }
}

fn ui_fixture_manifest() -> Value {
    let bytes = fs::read(fixture_path(UI_FIXTURE_MANIFEST)).expect("read UI fixture manifest");
    serde_json::from_slice(&bytes).expect("parse UI fixture manifest")
}

fn manifest_number(manifest: &Value, key: &str) -> usize {
    manifest[key]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("manifest field {key} must be a usize"))
}

fn manifest_string<'a>(manifest: &'a Value, key: &str) -> &'a str {
    manifest[key]
        .as_str()
        .unwrap_or_else(|| panic!("manifest field {key} must be a string"))
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn test_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kite-mindmap-{label}-{}-{}.xmind",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}
