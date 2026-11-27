//! Markdown to outline adapter.

use std::collections::HashMap;

use markdown::mdast::Node;
use markdown::{Constructs, ParseOptions};

use crate::mindmap_import::adapter_support::{
    HeadingStack, accumulate_report, decode_utf8, fallback_title, node_at_mut, provenance,
    source_location, truncate_title,
};
use crate::mindmap_import::model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode,
    ImportError, LinkKind, OutlineAdapter, OutlineAttachment, OutlineDocument, OutlineLink,
    OutlineNode, OutlineNote, OutlineTable, SourceFormat, SourceInput, SourceLocation,
    StructureKind, TaskState,
};
use crate::mindmap_import::segment::add_paragraph_as_content_nodes;

pub struct MarkdownAdapter;

impl OutlineAdapter for MarkdownAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            name: "markdown-mdast".to_string(),
            version: "1".to_string(),
            formats: vec![SourceFormat::Markdown],
        }
    }

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError> {
        options.validate()?;
        let text = decode_utf8(source.bytes)?;
        let tree = markdown::to_mdast(
            &text,
            &ParseOptions {
                constructs: Constructs {
                    frontmatter: true,
                    gfm_task_list_item: true,
                    gfm_table: true,
                    ..Constructs::gfm()
                },
                ..ParseOptions::default()
            },
        )
        .map_err(|error| ImportError::InvalidDocument(error.to_string()))?;
        let Node::Root(root) = tree else {
            return Err(ImportError::InvalidDocument(
                "Markdown parser did not return a document root".to_string(),
            ));
        };

        let definitions = collect_definitions(&root.children);
        let front_matter = root.children.iter().find_map(|node| match node {
            Node::Yaml(yaml) => Some(yaml.value.clone()),
            Node::Toml(toml) => Some(toml.value.clone()),
            _ => None,
        });
        let first_h1 = root.children.iter().find_map(|node| match node {
            Node::Heading(heading) if heading.depth == 1 => Some(inline_text(&heading.children)),
            _ => None,
        });
        let front_title = front_matter.as_deref().and_then(parse_front_matter_title);
        let title_from_h1 = front_title.is_none() && first_h1.is_some();
        let title = front_title
            .or(first_h1)
            .unwrap_or_else(|| fallback_title(source.path, "Untitled document"));

        let mut source_provenance =
            provenance(SourceFormat::Markdown, "markdown-mdast", source.path, &text);
        if let Some(front_matter) = front_matter.as_ref() {
            source_provenance.original_format.insert(
                "front_matter".to_string(),
                serde_json::Value::String(front_matter.clone()),
            );
        }
        let mut document = OutlineDocument::new(title, source_provenance);
        if let Some(front_matter) = front_matter.as_ref()
            && !front_matter.trim().is_empty()
        {
            document.notes.push(OutlineNote::with_label(
                front_matter.clone(),
                "Front matter",
                Some(source_location(
                    vec!["Front matter".to_string()],
                    Some(1),
                    None,
                    Some(0),
                )),
            ));
        }

        let mut processor = MarkdownProcessor {
            document: &mut document,
            options,
            definitions,
            stack: HeadingStack::default(),
            consume_first_h1_as_title: title_from_h1,
        };
        processor.process_blocks(&root.children, None);
        if document.node_count() == 0 && document.notes.is_empty() {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::EmptyInput,
                "Markdown document contained no convertible content",
                None,
            ));
        }
        Ok(document)
    }
}

struct MarkdownProcessor<'a> {
    document: &'a mut OutlineDocument,
    options: &'a ConversionOptions,
    definitions: HashMap<String, (String, Option<String>)>,
    stack: HeadingStack,
    consume_first_h1_as_title: bool,
}

impl MarkdownProcessor<'_> {
    fn process_blocks(&mut self, nodes: &[Node], forced_parent: Option<&[usize]>) {
        for node in nodes {
            match node {
                Node::Heading(heading) => {
                    if self.consume_first_h1_as_title && heading.depth == 1 {
                        self.consume_first_h1_as_title = false;
                        continue;
                    }
                    self.push_heading(heading, forced_parent);
                }
                Node::Paragraph(paragraph) => {
                    let parent = forced_parent
                        .map(ToOwned::to_owned)
                        .or_else(|| self.stack.current())
                        .unwrap_or_else(|| self.overview_path());
                    self.add_paragraph(&paragraph.children, &parent);
                }
                Node::List(list) => {
                    let parent = forced_parent
                        .map(ToOwned::to_owned)
                        .or_else(|| self.stack.current())
                        .unwrap_or_else(|| self.overview_path());
                    self.push_list(&list.children, list.ordered, 1, &parent);
                }
                Node::Code(code) => {
                    let parent = forced_parent
                        .map(ToOwned::to_owned)
                        .or_else(|| self.stack.current())
                        .unwrap_or_else(|| self.overview_path());
                    self.push_code(&parent, code);
                }
                Node::Table(table) => {
                    let parent = forced_parent
                        .map(ToOwned::to_owned)
                        .or_else(|| self.stack.current())
                        .unwrap_or_else(|| self.overview_path());
                    self.push_table(&parent, table);
                }
                Node::Blockquote(quote) => self.process_blocks(&quote.children, forced_parent),
                Node::Html(html) => {
                    let text = strip_html_tags(&html.value);
                    if !text.trim().is_empty() {
                        let parent = forced_parent
                            .map(ToOwned::to_owned)
                            .or_else(|| self.stack.current())
                            .unwrap_or_else(|| self.overview_path());
                        self.add_text_paragraph(&text, &parent, None);
                    }
                    self.document.warnings.push(ConversionWarning::warning(
                        ConversionWarningCode::UnsupportedElement,
                        "Raw Markdown HTML was flattened to plain text",
                        Some(source_location(
                            vec![],
                            position_line(html.position.as_ref()),
                            None,
                            None,
                        )),
                    ));
                }
                Node::ThematicBreak(_) | Node::Yaml(_) | Node::Toml(_) | Node::Definition(_) => {}
                Node::FootnoteDefinition(footnote) => {
                    let text = inline_text(&footnote.children);
                    if !text.trim().is_empty() {
                        let parent = forced_parent
                            .map(ToOwned::to_owned)
                            .or_else(|| self.stack.current())
                            .unwrap_or_else(|| self.overview_path());
                        let node = node_at_mut(self.document, &parent).expect("valid parent path");
                        node.notes.push(OutlineNote::with_label(
                            text,
                            format!("Footnote {}", footnote.label.as_deref().unwrap_or("")),
                            Some(source_location(
                                vec![],
                                position_line(footnote.position.as_ref()),
                                None,
                                footnote
                                    .position
                                    .as_ref()
                                    .map(|position| position.start.offset),
                            )),
                        ));
                    }
                }
                other => self.document.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::UnsupportedElement,
                    format!("Unsupported Markdown element: {other:?}"),
                    None,
                )),
            }
        }
    }

    fn push_heading(
        &mut self,
        heading: &markdown::mdast::Heading,
        forced_parent: Option<&[usize]>,
    ) {
        let parent = forced_parent
            .map(ToOwned::to_owned)
            .or_else(|| self.stack.parent_for(heading.depth));
        let raw_title = inline_text(&heading.children);
        let source = source_location(
            parent
                .as_ref()
                .map_or_else(Vec::new, |path| self.path_titles(path)),
            position_line(heading.position.as_ref()),
            None,
            heading
                .position
                .as_ref()
                .map(|position| position.start.offset),
        );
        let title = truncate_title(
            &raw_title,
            Some(source.clone()),
            self.options,
            &mut self.document.stats,
            &mut self.document.warnings,
        );
        let node = OutlineNode::structure(
            title,
            StructureKind::Heading {
                level: heading.depth,
            },
        )
        .with_source(source);
        let path = append_structure(self.document, parent.as_deref(), node);
        self.stack.record(heading.depth, path);
        if let Some(node) = node_at_mut(self.document, &self.stack.current().expect("heading path"))
        {
            node.links
                .extend(collect_links(&heading.children, &self.definitions));
        }
    }

    fn push_list(&mut self, items: &[Node], ordered: bool, level: u8, parent: &[usize]) {
        for item in items {
            let Node::ListItem(item) = item else {
                continue;
            };
            let mut item_node = OutlineNode::structure(
                "Untitled list item",
                StructureKind::ListItem { ordered, level },
            );
            item_node.task = item.checked.map(|checked| {
                if checked {
                    TaskState::Done
                } else {
                    TaskState::Todo
                }
            });
            let item_source = source_location(
                self.path_titles(parent),
                position_line(item.position.as_ref()),
                None,
                item.position.as_ref().map(|position| position.start.offset),
            );
            item_node.source = Some(item_source.clone());

            let mut first_paragraph = None;
            let mut remaining = Vec::new();
            for child in &item.children {
                if first_paragraph.is_none() {
                    if let Node::Paragraph(paragraph) = child {
                        first_paragraph = Some(paragraph);
                    } else {
                        remaining.push(child.clone());
                    }
                } else {
                    remaining.push(child.clone());
                }
            }
            if let Some(paragraph) = first_paragraph {
                let title = inline_text(&paragraph.children);
                item_node.title = truncate_title(
                    &title,
                    Some(item_source.clone()),
                    self.options,
                    &mut self.document.stats,
                    &mut self.document.warnings,
                );
                item_node
                    .links
                    .extend(collect_links(&paragraph.children, &self.definitions));
                if self.options.preserve_original_paragraphs && !title.trim().is_empty() {
                    item_node.notes.push(OutlineNote::with_label(
                        title.trim(),
                        "Original paragraph",
                        Some(item_source.clone()),
                    ));
                }
            }
            let item_path = append_structure(self.document, Some(parent), item_node);

            if let Some(paragraph) = first_paragraph {
                let images = collect_images(&paragraph.children, &self.definitions);
                self.append_images(&item_path, images, Some(item_source.clone()));
            }
            for child in &remaining {
                if let Node::List(list) = child {
                    self.push_list(
                        &list.children,
                        list.ordered,
                        level.saturating_add(1),
                        &item_path,
                    );
                } else {
                    self.process_blocks(std::slice::from_ref(child), Some(&item_path));
                }
            }
        }
    }

    fn push_code(&mut self, parent: &[usize], code: &markdown::mdast::Code) {
        let language = code.lang.as_deref().unwrap_or("text");
        let title = format!("Code block · {language}");
        let mut node = OutlineNode::content(title, ContentKind::CodeBlock);
        node.source = Some(source_location(
            self.path_titles(parent),
            position_line(code.position.as_ref()),
            None,
            code.position.as_ref().map(|position| position.start.offset),
        ));
        node.notes.push(OutlineNote::with_label(
            code.value.trim_end(),
            format!("Code ({language})"),
            node.source.clone(),
        ));
        node_at_mut(self.document, parent)
            .expect("valid code parent")
            .children
            .push(node);
        if code.value.trim().is_empty() {
            self.document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                "An empty code block was skipped",
                Some(source_location(
                    vec![],
                    position_line(code.position.as_ref()),
                    None,
                    None,
                )),
            ));
        }
    }

    fn push_table(&mut self, parent: &[usize], table: &markdown::mdast::Table) {
        let mut rows = Vec::new();
        for row in &table.children {
            let Node::TableRow(row) = row else {
                continue;
            };
            let cells = row
                .children
                .iter()
                .filter_map(|cell| match cell {
                    Node::TableCell(cell) => Some(inline_text(&cell.children)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            rows.push(cells);
        }
        let headers = if rows.is_empty() {
            Vec::new()
        } else {
            rows.remove(0)
        };
        let outline_table = OutlineTable {
            caption: None,
            headers,
            rows,
            source: Some(source_location(
                self.path_titles(parent),
                position_line(table.position.as_ref()),
                None,
                table
                    .position
                    .as_ref()
                    .map(|position| position.start.offset),
            )),
        };
        node_at_mut(self.document, parent)
            .expect("valid table parent")
            .tables
            .push(outline_table);
    }

    fn add_paragraph(&mut self, children: &[Node], parent: &[usize]) {
        let text = inline_text(children);
        let links = collect_links(children, &self.definitions);
        let images = collect_images(children, &self.definitions);
        let source = children
            .first()
            .and_then(|node| node.position())
            .map(|position| {
                source_location(
                    self.path_titles(parent),
                    Some(position.start.line),
                    Some(position.start.column),
                    Some(position.start.offset),
                )
            });
        self.add_text_paragraph_with_links(&text, parent, source.clone(), links);
        self.append_images(parent, images, source);
    }

    fn add_text_paragraph(&mut self, text: &str, parent: &[usize], source: Option<SourceLocation>) {
        self.add_text_paragraph_with_links(text, parent, source, Vec::new());
    }

    fn add_text_paragraph_with_links(
        &mut self,
        text: &str,
        parent: &[usize],
        source: Option<SourceLocation>,
        links: Vec<OutlineLink>,
    ) {
        let report = {
            let node = node_at_mut(self.document, parent).expect("valid paragraph parent");
            add_paragraph_as_content_nodes(node, text, source.clone(), self.options)
        };
        match report {
            Ok(report) => accumulate_report(self.document, report),
            Err(error) => self.document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::InvalidStructure,
                format!("Paragraph was not converted: {error}"),
                source,
            )),
        }
        if !links.is_empty() {
            node_at_mut(self.document, parent)
                .expect("valid paragraph parent")
                .links
                .extend(links);
        }
    }

    fn append_images(
        &mut self,
        parent: &[usize],
        images: Vec<(String, String, Option<String>)>,
        source: Option<SourceLocation>,
    ) {
        for (alt, url, title) in images {
            let label = if alt.trim().is_empty() {
                title.unwrap_or_else(|| "Image".to_string())
            } else {
                alt
            };
            let mut node = OutlineNode::content(
                format!("Image: {label}"),
                ContentKind::Other("image".to_string()),
            );
            node.source = source.clone();
            node.links.push(OutlineLink {
                label: Some(label),
                target: url.clone(),
                kind: LinkKind::Image,
                source: source.clone(),
            });
            node.attachments.push(OutlineAttachment {
                name: url
                    .rsplit('/')
                    .next()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("image")
                    .to_string(),
                media_type: None,
                uri: Some(url),
                data: None,
                source: source.clone(),
            });
            node_at_mut(self.document, parent)
                .expect("valid image parent")
                .children
                .push(node);
            self.document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                "Image is represented as a link; no external resource was fetched",
                source.clone(),
            ));
        }
    }

    fn overview_path(&mut self) -> Vec<usize> {
        if let Some(index) = self
            .document
            .children
            .iter()
            .position(|node| node.title == "Overview" && node.is_structure())
        {
            return vec![index];
        }
        self.document
            .children
            .push(OutlineNode::structure("Overview", StructureKind::Section));
        vec![self.document.children.len() - 1]
    }

    fn path_titles(&self, path: &[usize]) -> Vec<String> {
        let mut titles = Vec::new();
        let mut children = &self.document.children;
        for index in path {
            let Some(node) = children.get(*index) else {
                break;
            };
            titles.push(node.title.clone());
            children = &node.children;
        }
        titles
    }
}

fn append_structure(
    document: &mut OutlineDocument,
    parent: Option<&[usize]>,
    node: OutlineNode,
) -> Vec<usize> {
    if let Some(parent) = parent {
        let parent_node = node_at_mut(document, parent).expect("valid structural parent");
        parent_node.children.push(node);
        let mut path = parent.to_vec();
        path.push(parent_node.children.len() - 1);
        path
    } else {
        document.children.push(node);
        vec![document.children.len() - 1]
    }
}

fn collect_definitions(nodes: &[Node]) -> HashMap<String, (String, Option<String>)> {
    let mut definitions = HashMap::new();
    for node in nodes {
        if let Node::Definition(definition) = node {
            definitions.insert(
                definition.identifier.clone(),
                (definition.url.clone(), definition.title.clone()),
            );
        }
        if let Some(children) = node.children() {
            definitions.extend(collect_definitions(children));
        }
    }
    definitions
}

fn collect_links(
    nodes: &[Node],
    definitions: &HashMap<String, (String, Option<String>)>,
) -> Vec<OutlineLink> {
    let mut links = Vec::new();
    for node in nodes {
        match node {
            Node::Link(link) => links.push(OutlineLink {
                label: Some(inline_text(&link.children)),
                target: link.url.clone(),
                kind: LinkKind::Hyperlink,
                source: link.position.as_ref().map(|position| SourceLocation {
                    path: Vec::new(),
                    line: Some(position.start.line as u32),
                    column: Some(position.start.column as u32),
                    byte_offset: Some(position.start.offset),
                }),
            }),
            Node::LinkReference(reference) => {
                if let Some((url, _)) = definitions.get(&reference.identifier) {
                    links.push(OutlineLink {
                        label: reference
                            .label
                            .clone()
                            .or_else(|| Some(inline_text(&reference.children))),
                        target: url.clone(),
                        kind: LinkKind::Hyperlink,
                        source: reference.position.as_ref().map(|position| SourceLocation {
                            path: Vec::new(),
                            line: Some(position.start.line as u32),
                            column: Some(position.start.column as u32),
                            byte_offset: Some(position.start.offset),
                        }),
                    });
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    links.extend(collect_links(children, definitions));
                }
            }
        }
    }
    links
}

fn collect_images(
    nodes: &[Node],
    definitions: &HashMap<String, (String, Option<String>)>,
) -> Vec<(String, String, Option<String>)> {
    let mut images = Vec::new();
    for node in nodes {
        match node {
            Node::Image(image) => {
                images.push((image.alt.clone(), image.url.clone(), image.title.clone()));
            }
            Node::ImageReference(reference) => {
                if let Some((url, _)) = definitions.get(&reference.identifier) {
                    images.push((reference.alt.clone(), url.clone(), None));
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    images.extend(collect_images(children, definitions));
                }
            }
        }
    }
    images
}

fn inline_text(nodes: &[Node]) -> String {
    let mut output = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => output.push_str(&text.value),
            Node::InlineCode(code) => output.push_str(&code.value),
            Node::InlineMath(math) => output.push_str(&math.value),
            Node::Break(_) => output.push(' '),
            Node::Image(image) => {
                if !image.alt.is_empty() {
                    output.push_str(&image.alt);
                }
            }
            Node::ImageReference(image) => output.push_str(&image.alt),
            Node::Link(link) => output.push_str(&inline_text(&link.children)),
            Node::LinkReference(link) => output.push_str(&inline_text(&link.children)),
            Node::Emphasis(value) => output.push_str(&inline_text(&value.children)),
            Node::Strong(value) => output.push_str(&inline_text(&value.children)),
            Node::Delete(value) => output.push_str(&inline_text(&value.children)),
            Node::Html(html) => output.push_str(&strip_html_tags(&html.value)),
            Node::Code(code) => output.push_str(&code.value),
            other => {
                if let Some(children) = other.children() {
                    output.push_str(&inline_text(children));
                }
            }
        }
    }
    output.trim().to_string()
}

fn parse_front_matter_title(value: &str) -> Option<String> {
    value.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if !key.trim().eq_ignore_ascii_case("title") {
            return None;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn position_line(position: Option<&markdown::unist::Position>) -> Option<usize> {
    position.map(|position| position.start.line)
}

fn strip_html_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.trim().to_string()
}
