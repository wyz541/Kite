use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{NaiveDate, NaiveTime};
use kite_core::mindmap::{
    BorderLinePattern, DateDisplayFormat, DateInsertionError, DateInsertionPlacement,
    DateInsertionRequest, DateInsertionTarget, DateLocale, DatePrecision, FontFamily, FontSize,
    FontStyle, FontWeight, MissingFontAction, MissingFontPolicy, NodeCornerRadius, NodeStylePatch,
    StaticDateValue, StyleColor, StyleMeasurement, TextAlignment, TextStylePatch, TopicStyleError,
    TopicStylePatch, XmindPackage, apply_topic_style, insert_date_into_topic, read_topic_style,
};
use serde_json::{Value, json};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn applies_all_typed_styles_to_one_topic_and_preserves_siblings_and_unknowns() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let original = package.content().clone();
    let mut content = original.clone();
    let sibling_before = topic_by_id(&content[0]["rootTopic"], "sibling")
        .unwrap()
        .clone();
    let target = topic_mut_by_id(&mut content[0]["rootTopic"], "target").expect("target topic");

    let patch = TopicStylePatch {
        text: TextStylePatch {
            font_family: Some(FontFamily::new("Kite Industrial Mono, monospace").unwrap()),
            font_size: Some(FontSize::points(13.0).unwrap()),
            font_weight: Some(FontWeight::numeric(600).unwrap()),
            font_style: Some(FontStyle::Italic),
            underline: Some(false),
            strike: Some(true),
            color: Some(StyleColor::new("#112233").unwrap()),
            alignment: Some(TextAlignment::Left),
            line_height: Some(StyleMeasurement::unitless(1.4).unwrap()),
            letter_spacing: Some(StyleMeasurement::points(0.25).unwrap()),
        },
        node: NodeStylePatch {
            background: Some(StyleColor::new("#F6F8FA").unwrap()),
            border_color: Some(StyleColor::new("#D0D7DE").unwrap()),
            border_width: Some(StyleMeasurement::points(1.0).unwrap()),
            border_pattern: Some(BorderLinePattern::Solid),
            corner_radius: Some(NodeCornerRadius::new(8).unwrap()),
        },
    };
    apply_topic_style(target, &patch).expect("apply style");

    let style = topic_by_id(&content, "target").unwrap();
    assert_eq!(
        style["style"]["properties"]["fo:font-family"],
        "Kite Industrial Mono, monospace"
    );
    assert_eq!(style["style"]["properties"]["fo:font-size"], "13pt");
    assert_eq!(style["style"]["properties"]["fo:font-weight"], "600");
    assert_eq!(style["style"]["properties"]["fo:font-style"], "italic");
    assert_eq!(
        style["style"]["properties"]["fo:text-decoration"],
        "overline line-through"
    );
    assert_eq!(style["style"]["properties"]["fo:color"], "#112233");
    assert_eq!(style["style"]["properties"]["fo:text-align"], "left");
    assert_eq!(style["style"]["properties"]["fo:line-height"], "1.4");
    assert_eq!(style["style"]["properties"]["fo:letter-spacing"], "0.25pt");
    assert_eq!(style["style"]["properties"]["svg:fill"], "#F6F8FA");
    assert_eq!(style["style"]["properties"]["border-line-color"], "#D0D7DE");
    assert_eq!(style["style"]["properties"]["border-line-width"], "1pt");
    assert_eq!(style["style"]["properties"]["border-line-pattern"], "solid");
    assert_eq!(style["style"]["properties"]["shape-corner"], "8");
    assert_eq!(
        style["style"]["properties"]["unknown-property"],
        json!({"futureValue": true})
    );
    assert_eq!(style["style"]["futureStyleField"], json!(["kept"]));
    assert_eq!(style["futureTopicField"], json!({"kept": true}));
    assert_eq!(
        style["notes"],
        topic_by_id(&original, "target").unwrap()["notes"]
    );
    assert_eq!(
        topic_by_id(&content, "sibling").unwrap(),
        &sibling_before,
        "sibling topic must remain byte-for-byte equal as JSON"
    );

    let typed = read_topic_style(style).expect("read style");
    assert_eq!(typed.text.font_size, Some(FontSize::points(13.0).unwrap()));
    assert_eq!(
        typed.text.decoration,
        Some(kite_core::mindmap::TextDecoration {
            underline: false,
            strike: true,
        })
    );
    assert_eq!(
        typed.node.corner_radius,
        Some(NodeCornerRadius::new(8).unwrap())
    );

    package.replace_content(content);
    let path = temp_path("topic-style");
    package.save_as(&path).expect("save styled package");
    let reopened = XmindPackage::open(&path).expect("reopen styled package");
    let reopened_target = topic_by_id(reopened.content(), "target").unwrap();
    assert_eq!(
        reopened_target["style"]["properties"]["fo:font-family"],
        "Kite Industrial Mono, monospace"
    );
    assert_eq!(
        reopened_target["style"]["properties"]["unknown-property"],
        json!({"futureValue": true})
    );
    assert_eq!(
        topic_by_id(reopened.content(), "sibling").unwrap(),
        &sibling_before
    );
    let _ = fs::remove_file(path);
}

#[test]
fn real_ui_fixture_style_patch_changes_only_the_selected_topic_properties() {
    let package = XmindPackage::open(std::path::Path::new(
        "tests/fixtures/mindmap/kite-rust-gpui-capability.xmind",
    ))
    .expect("open UI fixture");
    let mut content = package.content().clone();
    let styled = styled_topic_ids(&content);
    assert!(
        styled.len() >= 2,
        "fixture must contain multiple styled topics"
    );
    let target_id = styled[0].clone();
    let other_id = styled[1].clone();
    let other_before = topic_by_id(&content, &other_id).unwrap().clone();
    let mut expected_properties = topic_by_id(&content, &target_id).unwrap()["style"]["properties"]
        .as_object()
        .unwrap()
        .clone();
    expected_properties.insert("fo:font-size".to_owned(), json!("12pt"));

    let target = topic_mut_by_id(&mut content[0]["rootTopic"], &target_id).unwrap();
    apply_topic_style(
        target,
        &TopicStylePatch {
            text: TextStylePatch {
                font_size: Some(FontSize::points(12.0).unwrap()),
                ..TextStylePatch::default()
            },
            ..TopicStylePatch::default()
        },
    )
    .expect("apply fixture style");

    let target = topic_by_id(&content, &target_id).unwrap();
    assert_eq!(
        target["style"]["properties"],
        Value::Object(expected_properties)
    );
    assert_eq!(topic_by_id(&content, &other_id).unwrap(), &other_before);
}

#[test]
fn inserts_dates_into_explicit_title_and_note_targets_without_touching_siblings() {
    let mut package = XmindPackage::from_bytes(synthetic_package()).expect("parse package");
    let original = package.content().clone();
    let mut content = original.clone();
    let sibling_before = topic_by_id(&content[0]["rootTopic"], "sibling")
        .unwrap()
        .clone();
    let date = StaticDateValue::new(NaiveDate::from_ymd_opt(2026, 11, 27).unwrap())
        .with_time(NaiveTime::from_hms_opt(14, 5, 9).unwrap());

    let title_request = DateInsertionRequest {
        value: date.clone(),
        format: DateDisplayFormat::ISO_DATE,
        target: DateInsertionTarget::Title,
        placement: DateInsertionPlacement::End,
    };
    let note_request = DateInsertionRequest {
        value: date,
        format: DateDisplayFormat::locale(DateLocale::Chinese, DatePrecision::Seconds),
        target: DateInsertionTarget::Note,
        placement: DateInsertionPlacement::Start,
    };

    let target = topic_mut_by_id(&mut content[0]["rootTopic"], "target").unwrap();
    insert_date_into_topic(target, &title_request).expect("insert title date");
    let inserted = insert_date_into_topic(target, &note_request).expect("insert note date");
    assert_eq!(inserted.text, "2026年11月27日 14:05:09");

    let target = topic_by_id(&content, "target").unwrap();
    assert_eq!(target["title"], "Target 2026-11-27");
    assert_eq!(
        target["notes"]["plain"]["content"],
        "2026年11月27日 14:05:09\nExisting note"
    );
    assert_eq!(
        target["notes"]["plain"]["futurePlainField"],
        json!({"kept": true})
    );
    assert_eq!(target["notes"]["futureNotesField"], 9);
    assert_eq!(target["futureTopicField"], json!({"kept": true}));
    assert_eq!(topic_by_id(&content, "sibling").unwrap(), &sibling_before);

    package.replace_content(content);
    let path = temp_path("topic-date");
    package.save_as(&path).expect("save dated package");
    let reopened = XmindPackage::open(&path).expect("reopen dated package");
    let target = topic_by_id(reopened.content(), "target").unwrap();
    assert_eq!(target["title"], "Target 2026-11-27");
    assert_eq!(
        target["notes"]["plain"]["content"],
        "2026年11月27日 14:05:09\nExisting note"
    );
    let _ = fs::remove_file(path);
}

#[test]
fn date_with_missing_time_is_rejected_atomically() {
    let mut topic = json!({
        "id": "target",
        "title": "Target",
        "future": {"kept": true}
    });
    let before = topic.clone();
    let request = DateInsertionRequest {
        value: StaticDateValue::new(NaiveDate::from_ymd_opt(2026, 11, 27).unwrap()),
        format: DateDisplayFormat::ISO_DATE_TIME,
        target: DateInsertionTarget::Title,
        placement: DateInsertionPlacement::End,
    };
    let error = insert_date_into_topic(&mut topic, &request).expect_err("missing time");
    assert!(matches!(
        error,
        DateInsertionError::Format(kite_core::mindmap::DateFormatError::MissingTime)
    ));
    assert_eq!(topic, before);
}

#[test]
fn style_errors_leave_topic_unchanged() {
    let mut topic = json!({
        "id": "target",
        "title": "Target",
        "style": {"properties": "future-shape"},
        "future": {"kept": true}
    });
    let before = topic.clone();
    let patch = TopicStylePatch {
        text: TextStylePatch {
            font_size: Some(FontSize::points(12.0).unwrap()),
            ..TextStylePatch::default()
        },
        ..TopicStylePatch::default()
    };
    let error = apply_topic_style(&mut topic, &patch).expect_err("invalid style shape");
    assert!(matches!(error, TopicStyleError::InvalidObject { .. }));
    assert_eq!(topic, before);
}

#[test]
fn missing_font_policy_never_rewrites_requested_family_implicitly() {
    let requested = FontFamily::new("Kite Industrial Mono").unwrap();
    let fallback = FontFamily::new("JetBrains Mono").unwrap();

    let preserve = MissingFontPolicy::PreserveRequested.resolve_missing(&requested);
    assert_eq!(
        preserve.warning.action,
        MissingFontAction::PreserveRequested
    );
    assert_eq!(preserve.warning.persisted_family, requested);
    assert!(preserve.warning.render_family.is_none());

    let substitute =
        MissingFontPolicy::SubstituteForLayout(fallback.clone()).resolve_missing(&requested);
    assert_eq!(
        substitute.warning.action,
        MissingFontAction::SubstituteForLayout
    );
    assert_eq!(substitute.warning.persisted_family, requested);
    assert_eq!(substitute.warning.render_family, Some(fallback));
}

fn synthetic_package() -> Vec<u8> {
    let content = json!([
        {
            "id": "sheet-1",
            "class": "sheet",
            "title": "Style and date fixture",
            "futureSheetField": {"kept": true},
            "rootTopic": {
                "id": "root",
                "class": "topic",
                "title": "Root",
                "children": {
                    "attached": [
                        {
                            "id": "target",
                            "class": "topic",
                            "title": "Target",
                            "futureTopicField": {"kept": true},
                            "style": {
                                "id": "target-style",
                                "type": "topic",
                                "futureStyleField": ["kept"],
                                "properties": {
                                    "fo:font-family": "Old Sans",
                                    "fo:font-size": "10pt",
                                    "fo:text-decoration": "overline underline",
                                    "unknown-property": {"futureValue": true}
                                }
                            },
                            "notes": {
                                "plain": {
                                    "content": "Existing note",
                                    "futurePlainField": {"kept": true}
                                },
                                "futureNotesField": 9
                            }
                        },
                        {
                            "id": "sibling",
                            "class": "topic",
                            "title": "Sibling",
                            "futureSiblingField": {"kept": true},
                            "style": {
                                "id": "sibling-style",
                                "type": "topic",
                                "properties": {
                                    "fo:font-size": "14pt",
                                    "sibling-only-property": "kept"
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
            serde_json::to_vec(&content).unwrap().as_slice(),
        ),
        ("resources/future.bin", b"future-entry".as_slice()),
    ])
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    output.into_inner()
}

fn topic_by_id<'a>(topic: &'a Value, id: &str) -> Option<&'a Value> {
    if let Some(values) = topic.as_array() {
        return values.iter().find_map(|value| topic_by_id(value, id));
    }
    if topic.get("id").and_then(Value::as_str) == Some(id) {
        return Some(topic);
    }
    if let Some(root) = topic.get("rootTopic")
        && let Some(found) = topic_by_id(root, id)
    {
        return Some(found);
    }
    let children = topic.get("children").and_then(Value::as_object)?;
    for values in children.values().filter_map(Value::as_array) {
        for child in values {
            if let Some(found) = topic_by_id(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn topic_mut_by_id<'a>(topic: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if topic.get("id").and_then(Value::as_str) == Some(id) {
        return Some(topic);
    }
    let children = topic.get_mut("children")?.as_object_mut()?;
    for values in children.values_mut().filter_map(Value::as_array_mut) {
        for child in values {
            if child.get("id").and_then(Value::as_str) == Some(id) {
                return Some(child);
            }
            if let Some(found) = topic_mut_by_id(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn styled_topic_ids(content: &Value) -> Vec<String> {
    fn collect(topic: &Value, output: &mut Vec<String>) {
        if let Some(values) = topic.as_array() {
            for value in values {
                collect(value, output);
            }
            return;
        }
        if topic.get("style").and_then(Value::as_object).is_some()
            && let Some(id) = topic.get("id").and_then(Value::as_str)
        {
            output.push(id.to_owned());
        }
        if let Some(root) = topic.get("rootTopic") {
            collect(root, output);
        }
        if let Some(children) = topic.get("children").and_then(Value::as_object) {
            for child in children.values().filter_map(Value::as_array).flatten() {
                collect(child, output);
            }
        }
    }

    let mut output = Vec::new();
    collect(content, &mut output);
    output
}

fn temp_path(label: &str) -> PathBuf {
    let unique = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kite-{label}-{}-{unique}.xmind",
        std::process::id()
    ))
}
