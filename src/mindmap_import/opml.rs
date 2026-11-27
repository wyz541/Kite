//! OPML to outline adapter.

use roxmltree::{Document, Node};

use crate::mindmap_import::adapter_support::{
    decode_utf8, fallback_title, provenance, source_location, truncate_title,
};
use crate::mindmap_import::model::{
    AdapterDescriptor, ConversionOptions, ConversionWarning, ConversionWarningCode, ImportError,
    LinkKind, NoteFormat, OutlineAdapter, OutlineDocument, OutlineLink, OutlineNode, OutlineNote,
    SourceFormat, SourceInput, SourceLocation, StructureKind, TaskState,
};

pub struct OpmlAdapter;

impl OutlineAdapter for OpmlAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            name: "opml-xml".to_string(),
            version: "1".to_string(),
            formats: vec![SourceFormat::Opml],
        }
    }

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError> {
        options.validate()?;
        let text = decode_utf8(source.bytes)?;
        let xml = Document::parse(&text).map_err(|error| {
            ImportError::InvalidDocument(format!(
                "invalid OPML XML at line {}, column {}: {error}",
                error.pos().row,
                error.pos().col
            ))
        })?;
        let root = xml.root_element();
        if !root.tag_name().name().eq_ignore_ascii_case("opml") {
            return Err(ImportError::InvalidDocument(format!(
                "expected OPML root element, found <{}>",
                root.tag_name().name()
            )));
        }

        let head_title = child_named(root, "head")
            .and_then(|head| child_named(head, "title"))
            .map(node_text)
            .filter(|value| !value.trim().is_empty());
        let title = head_title.unwrap_or_else(|| fallback_title(source.path, "Untitled document"));
        let mut document = OutlineDocument::new(
            title,
            provenance(SourceFormat::Opml, "opml-xml", source.path, &text),
        );

        let Some(body) = child_named(root, "body") else {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::InvalidStructure,
                "OPML document has no <body> element",
                document_source_location(&xml, root),
            ));
            return Ok(document);
        };

        let mut children = Vec::new();
        {
            let mut processor = OpmlProcessor {
                document: &mut document,
                options,
                xml: &xml,
                imported_nodes: 0,
                limit_warned: false,
            };
            let outlines = body
                .children()
                .filter(|node| node.is_element())
                .filter(|node| node.tag_name().name().eq_ignore_ascii_case("outline"))
                .collect::<Vec<_>>();
            for outline in outlines {
                processor.import_outline(outline, &mut children, 1);
            }
        }
        document.children = children;
        if body
            .children()
            .filter(|node| node.is_element())
            .any(|node| !node.tag_name().name().eq_ignore_ascii_case("outline"))
        {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                "Ignored non-outline elements directly inside <body>",
                document_source_location(&xml, body),
            ));
        }
        if document.node_count() == 0 {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::EmptyInput,
                "OPML document contained no outline nodes",
                document_source_location(&xml, body),
            ));
        }
        Ok(document)
    }
}

struct OpmlProcessor<'a, 'input> {
    document: &'a mut OutlineDocument,
    options: &'a ConversionOptions,
    xml: &'a Document<'input>,
    imported_nodes: usize,
    limit_warned: bool,
}

impl OpmlProcessor<'_, '_> {
    fn import_outline(&mut self, outline: Node<'_, '_>, parent: &mut Vec<OutlineNode>, depth: u8) {
        if self.imported_nodes >= self.options.max_nodes {
            if !self.limit_warned {
                self.document.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::NodeLimitExceeded,
                    format!(
                        "OPML import stopped at the configured {} node limit",
                        self.options.max_nodes
                    ),
                    Some(self.location(outline)),
                ));
                self.limit_warned = true;
            }
            return;
        }
        if usize::from(depth) > usize::from(self.options.max_depth) {
            self.document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::DepthLimitExceeded,
                format!(
                    "Skipped OPML subtree deeper than the configured {} levels",
                    self.options.max_depth
                ),
                Some(self.location(outline)),
            ));
            return;
        }

        let raw_title = outline
            .attribute("text")
            .or_else(|| outline.attribute("title"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Untitled topic");
        let source = self.location(outline);
        let title = truncate_title(
            raw_title,
            Some(source.clone()),
            self.options,
            &mut self.document.stats,
            &mut self.document.warnings,
        );
        let mut node =
            OutlineNode::structure(title, StructureKind::OutlineItem).with_source(source.clone());
        node.task = task_state(outline);

        for (name, kind) in [
            ("url", LinkKind::Hyperlink),
            ("htmlUrl", LinkKind::Hyperlink),
            ("xmlUrl", LinkKind::Attachment),
        ] {
            if let Some(target) = outline
                .attribute(name)
                .filter(|value| !value.trim().is_empty())
            {
                node.links.push(OutlineLink {
                    label: Some(format!("OPML {name}")),
                    target: target.to_string(),
                    kind,
                    source: Some(source.clone()),
                });
            }
        }
        if let Some(link) = outline
            .attribute("link")
            .filter(|value| !value.trim().is_empty())
        {
            node.links.push(OutlineLink {
                label: Some("OPML link".to_string()),
                target: link.to_string(),
                kind: LinkKind::Hyperlink,
                source: Some(source.clone()),
            });
        }

        for attribute in ["_note", "note", "description"] {
            if let Some(note) = outline
                .attribute(attribute)
                .filter(|value| !value.trim().is_empty())
            {
                node.notes.push(OutlineNote {
                    text: note.to_string(),
                    format: if contains_markup(note) {
                        NoteFormat::Html
                    } else {
                        NoteFormat::Plain
                    },
                    label: Some(attribute.to_string()),
                    source: Some(source.clone()),
                });
            }
        }
        for child in outline.children().filter(|child| child.is_element()) {
            match child.tag_name().name() {
                "note" => {
                    let note = node_text(child);
                    if !note.trim().is_empty() {
                        node.notes.push(OutlineNote {
                            text: note,
                            format: NoteFormat::Plain,
                            label: Some("note".to_string()),
                            source: Some(self.location(child)),
                        });
                    }
                }
                "outline" => {}
                other => self.document.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::UnsupportedElement,
                    format!("Ignored unsupported OPML element <{other}>"),
                    Some(self.location(child)),
                )),
            }
        }

        let mut children = Vec::new();
        for child in outline.children().filter(|child| child.is_element()) {
            if child.tag_name().name().eq_ignore_ascii_case("outline") {
                self.import_outline(child, &mut children, depth.saturating_add(1));
            }
        }
        node.children = children;
        parent.push(node);
        self.imported_nodes += 1;
    }

    fn location(&self, node: Node<'_, '_>) -> SourceLocation {
        let range = node.range();
        let position = self.xml.text_pos_at(range.start);
        source_location(
            Vec::new(),
            Some(position.row as usize),
            Some(position.col as usize),
            Some(range.start),
        )
    }
}

fn child_named<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name().eq_ignore_ascii_case(name))
}

fn node_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn document_source_location(document: &Document<'_>, node: Node<'_, '_>) -> Option<SourceLocation> {
    let range = node.range();
    let position = document.text_pos_at(range.start);
    Some(source_location(
        Vec::new(),
        Some(position.row as usize),
        Some(position.col as usize),
        Some(range.start),
    ))
}

fn task_state(node: Node<'_, '_>) -> Option<TaskState> {
    let value = node
        .attribute("_task")
        .or_else(|| node.attribute("task"))
        .or_else(|| node.attribute("checked"))?;
    match value.trim().to_ascii_lowercase().as_str() {
        "done" | "complete" | "completed" | "true" | "checked" => Some(TaskState::Done),
        "todo" | "open" | "false" | "unchecked" => Some(TaskState::Todo),
        _ => None,
    }
}

fn contains_markup(value: &str) -> bool {
    value.contains('<') && value.contains('>')
}
