use std::path::Path;

use kite_core::mindmap::XmindPackage;
use kite_core::mindmap_import::{
    ContentKind, ConversionOptions, ConversionWarningCode, HtmlAdapter, LinkKind, MarkdownAdapter,
    NoteFormat, OpmlAdapter, OutlineAdapter, OutlineDocument, OutlineNode, SourceFormat,
    SourceInput, StructureKind, TaskState, write_xmind_bytes,
};

fn extract_markdown(input: &str) -> OutlineDocument {
    let format = SourceFormat::Markdown;
    MarkdownAdapter
        .extract(
            SourceInput {
                path: Some(Path::new("release-plan.md")),
                bytes: input.as_bytes(),
                format: &format,
            },
            &ConversionOptions {
                min_fragment_chars: 1,
                ..ConversionOptions::default()
            },
        )
        .expect("Markdown extraction")
}

fn extract_html(input: &str) -> OutlineDocument {
    let format = SourceFormat::Html;
    HtmlAdapter
        .extract(
            SourceInput {
                path: Some(Path::new("guide.html")),
                bytes: input.as_bytes(),
                format: &format,
            },
            &ConversionOptions {
                min_fragment_chars: 1,
                ..ConversionOptions::default()
            },
        )
        .expect("HTML extraction")
}

fn extract_opml(input: &str) -> OutlineDocument {
    let format = SourceFormat::Opml;
    OpmlAdapter
        .extract(
            SourceInput {
                path: Some(Path::new("feeds.opml")),
                bytes: input.as_bytes(),
                format: &format,
            },
            &ConversionOptions::default(),
        )
        .expect("OPML extraction")
}

fn child<'a>(node: &'a OutlineNode, title: &str) -> &'a OutlineNode {
    node.children
        .iter()
        .find(|child| child.title == title)
        .unwrap_or_else(|| panic!("missing child {title:?}"))
}

fn top<'a>(document: &'a OutlineDocument, title: &str) -> &'a OutlineNode {
    document
        .children
        .iter()
        .find(|child| child.title == title)
        .unwrap_or_else(|| panic!("missing top-level topic {title:?}"))
}

#[test]
fn markdown_maps_headings_lists_paragraphs_code_tables_images_and_links() {
    let document = extract_markdown(
        r#"# Release Plan

Intro paragraph. It has two sentences;
## Scope

- [ ] Add adapter
  - [x] Cover Markdown
- [ ] Ship tests

### Details

See [the design](https://example.com/design). Another sentence.

```rust
fn main() {}
```

| Column | Value |
| --- | --- |
| Nodes | 42 |

![architecture](https://example.com/arch.png)

#### Deep

##### Deeper

###### Deepest

# Second Root

Second root body.
"#,
    );

    assert_eq!(document.title, "Release Plan");
    assert_eq!(document.children[0].title, "Overview");
    assert_eq!(document.children[0].children[0].title, "Intro paragraph.");
    assert_eq!(
        document.children[0].children[1].title,
        "It has two sentences;"
    );

    let scope = top(&document, "Scope");
    assert!(matches!(
        scope.kind,
        kite_core::mindmap_import::OutlineNodeKind::Structure {
            kind: StructureKind::Heading { level: 2 }
        }
    ));
    let todo = child(scope, "Add adapter");
    assert_eq!(todo.task, Some(TaskState::Todo));
    assert_eq!(child(todo, "Cover Markdown").task, Some(TaskState::Done));

    let details = child(scope, "Details");
    let link = details.links.first().expect("paragraph link");
    assert_eq!(link.target, "https://example.com/design");
    assert_eq!(link.kind, LinkKind::Hyperlink);
    assert!(
        details
            .notes
            .iter()
            .any(|note| note.text.contains("See the design"))
    );
    assert!(details.children.iter().any(|node| {
        matches!(
            node.kind,
            kite_core::mindmap_import::OutlineNodeKind::Content {
                kind: ContentKind::CodeBlock,
                ..
            }
        )
    }));
    assert_eq!(details.tables.len(), 1);
    assert_eq!(details.tables[0].headers, vec!["Column", "Value"]);
    let image = details
        .children
        .iter()
        .find(|node| node.title == "Image: architecture")
        .expect("image topic");
    assert_eq!(image.links[0].kind, LinkKind::Image);
    let deep = child(details, "Deep");
    let deeper = child(deep, "Deeper");
    assert_eq!(child(deeper, "Deepest").title, "Deepest");

    let second = top(&document, "Second Root");
    assert_eq!(second.children[0].title, "Second root body.");
    assert!(document.warnings.iter().any(|warning| {
        warning.code == ConversionWarningCode::UnsupportedElement
            && warning.message.contains("Image")
    }));

    let written = write_xmind_bytes(&document, &ConversionOptions::default()).expect("write XMind");
    let package = XmindPackage::from_bytes(written.bytes).expect("valid produced package");
    let todo_json =
        find_topic(&package.content()[0]["rootTopic"], "Add adapter").expect("task topic");
    let todo_notes = todo_json["notes"]["plain"]["content"]
        .as_str()
        .expect("task status note");
    assert!(todo_notes.contains("Task status: todo"));
}

#[test]
fn markdown_front_matter_title_is_root_and_h1_remains_a_topic() {
    let document = extract_markdown(
        r#"---
title: "Metadata Title"
owner: Kite
---
# Visible Heading

Body.
"#,
    );
    assert_eq!(document.title, "Metadata Title");
    assert_eq!(document.children[0].title, "Visible Heading");
    assert!(document.notes.iter().any(|note| {
        note.label.as_deref() == Some("Front matter") && note.text.contains("owner: Kite")
    }));
    assert!(document.source.original_format.contains_key("front_matter"));
}

#[test]
fn markdown_link_references_and_empty_or_malformed_input_remain_safe() {
    let document = extract_markdown(
        r#"# Refs

Read [the guide][guide] and ![diagram][diagram].

[guide]: https://example.com/guide
[diagram]: https://example.com/diagram.svg
"#,
    );
    let refs = top(&document, "Overview");
    assert!(
        refs.links
            .iter()
            .any(|link| link.target == "https://example.com/guide")
    );
    assert!(refs.children.iter().any(|node| {
        node.links
            .iter()
            .any(|link| link.target == "https://example.com/diagram.svg")
    }));

    let malformed = extract_markdown("[unclosed and `code");
    assert_eq!(malformed.title, "release-plan");
    assert!(malformed.node_count() > 0);
    assert!(
        !malformed
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::EmptyInput })
    );

    let empty = extract_markdown("   \n\n");
    assert!(
        empty
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::EmptyInput })
    );
}

#[test]
fn html_maps_semantics_and_ignores_active_or_navigation_regions() {
    let document = extract_html(
        r#"<!doctype html>
<html>
<head><title>Operations Guide</title><style>body { color: red; }</style></head>
<body>
<nav><p>Navigation text.</p></nav>
<main>
<h1>Runbook</h1>
<p>Check service health. Then inspect logs;</p>
<ul>
  <li data-task="todo">Restart worker <input type="checkbox"><ul><li>Confirm queue is empty.</li></ul></li>
</ul>
<table><caption>Commands</caption><tr><th>Name</th><th>Value</th></tr><tr><td>status</td><td>ok</td></tr></table>
<p><a href="https://example.com/runbook">Open runbook</a></p>
<img src="https://example.com/topology.png" alt="Topology">
<pre><code class="language-shell">kite status</code></pre>
</main>
<footer>Footer text.</footer>
<script>alert('not imported')</script>
</body>
</html>"#,
    );

    assert_eq!(document.title, "Operations Guide");
    let runbook = top(&document, "Runbook");
    let paragraph = &runbook.children[0];
    assert_eq!(paragraph.title, "Check service health.");
    assert_eq!(runbook.children[1].title, "Then inspect logs;");
    let task = child(runbook, "Restart worker");
    assert_eq!(task.task, Some(TaskState::Todo));
    assert_eq!(
        child(task, "Confirm queue is empty.").title,
        "Confirm queue is empty."
    );
    assert_eq!(runbook.tables[0].headers, vec!["Name", "Value"]);
    assert!(
        runbook
            .links
            .iter()
            .any(|link| link.target == "https://example.com/runbook")
    );
    assert!(
        runbook
            .children
            .iter()
            .any(|node| node.title == "Image: Topology")
    );
    assert!(
        runbook
            .children
            .iter()
            .any(|node| node.title == "Code block · shell")
    );
    for tag in ["script", "style", "nav", "footer"] {
        assert!(document.warnings.iter().any(|warning| {
            warning.code == ConversionWarningCode::UnsupportedElement
                && warning.message.contains(tag)
        }));
    }
    assert!(!to_plain_text(&document).contains("Navigation text"));
    assert!(!to_plain_text(&document).contains("Footer text"));
    assert!(!to_plain_text(&document).contains("alert('not imported')"));
}

#[test]
fn html_tolerates_malformed_markup_and_handles_special_content() {
    let document = extract_html(
        r#"<html><body><h1>Broken &amp; Tolerant<p>First sentence. Second sentence;<p>Third &lt;value&gt;<table><tr><th>A<th>B<tr><td>1<td>2</table>"#,
    );
    assert_eq!(document.title, "Broken & Tolerant");
    let broken = top(&document, "Overview");
    assert!(
        broken
            .children
            .iter()
            .any(|node| node.title == "First sentence.")
    );
    assert!(
        broken
            .children
            .iter()
            .any(|node| node.title == "Second sentence;")
    );
    assert!(
        broken
            .children
            .iter()
            .any(|node| node.title == "Third <value>")
    );
    assert_eq!(broken.tables[0].headers, vec!["A", "B"]);
    assert_eq!(
        broken.tables[0].rows,
        vec![vec!["1".to_string(), "2".to_string()]]
    );

    let script_only =
        extract_html("<html><head><script>let x = 1;</script></head><body></body></html>");
    assert!(
        script_only
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::EmptyInput })
    );
    assert!(
        script_only
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::UnsupportedElement })
    );
}

#[test]
fn opml_maps_direct_hierarchy_and_preserves_notes_links_and_tasks() {
    let document = extract_opml(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head><title>Engineering Feeds</title></head>
  <body>
    <outline text="Language" _note="Core language links." url="https://example.com/lang" _task="todo">
      <outline text="Rust" htmlUrl="https://www.rust-lang.org/" xmlUrl="https://blog.rust-lang.org/feed.xml">
        <note>Rust release feed.</note>
      </outline>
      <outline text="Swift" checked="true" />
    </outline>
    <outline title="Product" link="https://example.com/product" />
  </body>
</opml>"#,
    );

    assert_eq!(document.title, "Engineering Feeds");
    let language = top(&document, "Language");
    assert_eq!(language.task, Some(TaskState::Todo));
    assert_eq!(language.links[0].target, "https://example.com/lang");
    assert!(language.notes.iter().any(|note| {
        note.label.as_deref() == Some("_note")
            && note.text == "Core language links."
            && note.format == NoteFormat::Plain
    }));
    let rust = child(language, "Rust");
    assert_eq!(rust.links.len(), 2);
    assert!(rust.links.iter().any(|link| {
        link.target == "https://blog.rust-lang.org/feed.xml" && link.kind == LinkKind::Attachment
    }));
    assert_eq!(rust.notes[0].text, "Rust release feed.");
    assert_eq!(child(language, "Swift").task, Some(TaskState::Done));
    assert_eq!(
        top(&document, "Product").links[0].target,
        "https://example.com/product"
    );

    let written = write_xmind_bytes(&document, &ConversionOptions::default()).expect("write XMind");
    XmindPackage::from_bytes(written.bytes).expect("valid produced package");
}

#[test]
fn opml_parse_errors_and_limits_are_explicit() {
    let format = SourceFormat::Opml;
    let error = OpmlAdapter
        .extract(
            SourceInput {
                path: None,
                bytes: b"<opml><body><outline text='broken'></body></opml>".as_slice(),
                format: &format,
            },
            &ConversionOptions::default(),
        )
        .expect_err("malformed XML must fail");
    assert!(error.to_string().contains("invalid OPML XML"));

    let empty = extract_opml("<opml><head><title>Empty</title></head><body /></opml>");
    assert!(
        empty
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::EmptyInput })
    );

    let deep = r#"<opml><body><outline text="1"><outline text="2"><outline text="3"/></outline></outline></body></opml>"#;
    let format = SourceFormat::Opml;
    let limited = OpmlAdapter
        .extract(
            SourceInput {
                path: None,
                bytes: deep.as_bytes(),
                format: &format,
            },
            &ConversionOptions {
                max_depth: 2,
                ..ConversionOptions::default()
            },
        )
        .expect("limited depth extraction");
    assert!(
        limited
            .warnings
            .iter()
            .any(|warning| { warning.code == ConversionWarningCode::DepthLimitExceeded })
    );
}

fn find_topic<'a>(topic: &'a serde_json::Value, title: &str) -> Option<&'a serde_json::Value> {
    if topic.get("title").and_then(serde_json::Value::as_str) == Some(title) {
        return Some(topic);
    }
    topic
        .get("children")?
        .get("attached")?
        .as_array()?
        .iter()
        .find_map(|child| find_topic(child, title))
}

fn to_plain_text(document: &OutlineDocument) -> String {
    fn visit(node: &OutlineNode, output: &mut String) {
        output.push_str(&node.title);
        output.push('\n');
        for note in &node.notes {
            output.push_str(&note.text);
            output.push('\n');
        }
        for child in &node.children {
            visit(child, output);
        }
    }
    let mut output = String::new();
    for node in &document.children {
        visit(node, &mut output);
    }
    output
}
