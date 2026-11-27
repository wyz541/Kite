//! Tolerance-oriented HTML to outline adapter.
//!
//! This is deliberately a semantic reader, not a browser. It never resolves
//! URLs, executes scripts, loads styles, or fetches external resources.

use std::collections::HashMap;

use crate::mindmap_import::adapter_support::{
    HeadingStack, accumulate_report, decode_utf8, fallback_title, node_at_mut, provenance,
    truncate_title,
};
use crate::mindmap_import::model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode,
    ImportError, LinkKind, OutlineAdapter, OutlineAttachment, OutlineDocument, OutlineLink,
    OutlineNode, OutlineNote, OutlineTable, SourceFormat, SourceInput, SourceLocation,
    StructureKind, TaskState,
};
use crate::mindmap_import::segment::add_paragraph_as_content_nodes;

pub struct HtmlAdapter;

impl OutlineAdapter for HtmlAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            name: "html-semantic".to_string(),
            version: "1".to_string(),
            formats: vec![SourceFormat::Html],
        }
    }

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError> {
        options.validate()?;
        let text = decode_utf8(source.bytes)?;
        let parsed = parse_html(&text);
        let title_element = find_element(&parsed.nodes, "title")
            .map(|element| element_text(element, &mut Vec::new()))
            .filter(|value| !value.trim().is_empty());
        let first_h1 = find_element(&parsed.nodes, "h1")
            .map(|element| element_text(element, &mut Vec::new()))
            .filter(|value| !value.trim().is_empty());
        let title_from_h1 = title_element.is_none() && first_h1.is_some();
        let title = title_element
            .or(first_h1)
            .unwrap_or_else(|| fallback_title(source.path, "Untitled document"));
        let mut document = OutlineDocument::new(
            title,
            provenance(SourceFormat::Html, "html-semantic", source.path, &text),
        );
        document.source.original_format.insert(
            "parser".to_string(),
            serde_json::Value::String("tolerant-html".to_string()),
        );

        let mut processor = HtmlProcessor {
            document: &mut document,
            options,
            stack: HeadingStack::default(),
            consume_first_h1_as_title: title_from_h1,
        };
        processor.process_nodes(&parsed.nodes, None);
        for warning in parsed.warnings {
            document.warnings.push(warning);
        }
        if document.node_count() == 0 {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::EmptyInput,
                "HTML document contained no convertible body content",
                None,
            ));
        }
        Ok(document)
    }
}

struct HtmlProcessor<'a> {
    document: &'a mut OutlineDocument,
    options: &'a ConversionOptions,
    stack: HeadingStack,
    consume_first_h1_as_title: bool,
}

impl HtmlProcessor<'_> {
    fn process_nodes(&mut self, nodes: &[DomNode], forced_parent: Option<&[usize]>) {
        for node in nodes {
            match node {
                DomNode::Text(text) if !text.trim().is_empty() && forced_parent.is_none() => {
                    let parent = self.stack.current().unwrap_or_else(|| self.overview_path());
                    self.add_text_paragraph(text, &parent, None);
                }
                DomNode::Element(element) => self.process_element(element, forced_parent),
                _ => {}
            }
        }
    }

    fn process_element(&mut self, element: &Element, forced_parent: Option<&[usize]>) {
        let name = element.name.as_str();
        if is_ignored_region(name) {
            self.document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                format!("Ignored <{name}> region (no script, style, navigation, or footer content was imported)"),
                Some(element.source.clone()),
            ));
            return;
        }
        if name == "head" || name == "title" || name == "meta" || name == "link" {
            return;
        }
        if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            let depth = name[1..].parse::<u8>().unwrap_or(6);
            if self.consume_first_h1_as_title && depth == 1 {
                self.consume_first_h1_as_title = false;
                return;
            }
            self.push_heading(element, depth, forced_parent);
            return;
        }
        if matches!(name, "ul" | "ol") {
            let parent = forced_parent
                .map(ToOwned::to_owned)
                .or_else(|| self.stack.current())
                .unwrap_or_else(|| self.overview_path());
            self.push_list(element, name == "ol", 1, &parent);
            return;
        }
        match name {
            "p" | "blockquote" | "aside" | "figcaption" => {
                let parent = forced_parent
                    .map(ToOwned::to_owned)
                    .or_else(|| self.stack.current())
                    .unwrap_or_else(|| self.overview_path());
                self.add_element_paragraph(element, &parent);
            }
            "pre" => {
                let parent = forced_parent
                    .map(ToOwned::to_owned)
                    .or_else(|| self.stack.current())
                    .unwrap_or_else(|| self.overview_path());
                self.push_code(element, &parent);
            }
            "table" => {
                let parent = forced_parent
                    .map(ToOwned::to_owned)
                    .or_else(|| self.stack.current())
                    .unwrap_or_else(|| self.overview_path());
                self.push_table(element, &parent);
            }
            "img" => {
                let parent = forced_parent
                    .map(ToOwned::to_owned)
                    .or_else(|| self.stack.current())
                    .unwrap_or_else(|| self.overview_path());
                self.push_image(element, &parent, None);
            }
            "a" => {
                let parent = forced_parent
                    .map(ToOwned::to_owned)
                    .or_else(|| self.stack.current())
                    .unwrap_or_else(|| self.overview_path());
                self.add_element_paragraph(element, &parent);
            }
            "body" | "main" | "article" | "section" | "div" | "header" | "span" => {
                self.process_nodes(&element.children, forced_parent);
            }
            "dl" => {
                for child in &element.children {
                    if let DomNode::Element(child) = child
                        && matches!(child.name.as_str(), "dt" | "dd")
                    {
                        let parent = forced_parent
                            .map(ToOwned::to_owned)
                            .or_else(|| self.stack.current())
                            .unwrap_or_else(|| self.overview_path());
                        self.add_element_paragraph(child, &parent);
                    }
                }
            }
            "br" | "hr" | "source" | "input" => {}
            _ => {
                self.document.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::UnsupportedElement,
                    format!("Container <{name}> was traversed without a direct semantic mapping"),
                    Some(element.source.clone()),
                ));
                self.process_nodes(&element.children, forced_parent);
            }
        }
    }

    fn push_heading(&mut self, element: &Element, depth: u8, forced_parent: Option<&[usize]>) {
        let parent = forced_parent
            .map(ToOwned::to_owned)
            .or_else(|| self.stack.parent_for(depth));
        let raw_title = element_text(element, &mut Vec::new());
        let title = truncate_title(
            &raw_title,
            Some(element.source.clone()),
            self.options,
            &mut self.document.stats,
            &mut self.document.warnings,
        );
        let mut node = OutlineNode::structure(title, StructureKind::Heading { level: depth })
            .with_source(element.source.clone());
        node.links.extend(collect_links(element));
        let path = append_structure(self.document, parent.as_deref(), node);
        self.stack.record(depth, path);
    }

    fn push_list(&mut self, list: &Element, ordered: bool, level: u8, parent: &[usize]) {
        for child in &list.children {
            let DomNode::Element(item) = child else {
                continue;
            };
            if item.name != "li" {
                continue;
            }
            let title_text = list_item_text(item);
            let title = truncate_title(
                &title_text,
                Some(item.source.clone()),
                self.options,
                &mut self.document.stats,
                &mut self.document.warnings,
            );
            let mut node =
                OutlineNode::structure(title, StructureKind::ListItem { ordered, level })
                    .with_source(item.source.clone());
            node.task = task_state(item);
            node.links.extend(collect_links(item));
            if self.options.preserve_original_paragraphs && !title_text.trim().is_empty() {
                node.notes.push(OutlineNote::with_label(
                    title_text.trim(),
                    "Original paragraph",
                    Some(item.source.clone()),
                ));
            }
            let item_path = append_structure(self.document, Some(parent), node);
            self.process_nodes(&item.children, Some(&item_path));
        }
    }

    fn add_element_paragraph(&mut self, element: &Element, parent: &[usize]) {
        let text = element_text(element, &mut Vec::new());
        let links = collect_links(element);
        let images = collect_images(element);
        self.add_text_paragraph_with_links(&text, parent, Some(element.source.clone()), links);
        self.append_images(parent, images, Some(element.source.clone()));
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

    fn push_code(&mut self, element: &Element, parent: &[usize]) {
        let code = raw_element_text(element);
        let language = element
            .children
            .iter()
            .find_map(|child| match child {
                DomNode::Element(code) if code.name == "code" => code.attr("class"),
                _ => None,
            })
            .and_then(|class| {
                class
                    .split_whitespace()
                    .find_map(|value| value.strip_prefix("language-"))
            })
            .unwrap_or("text");
        let mut node =
            OutlineNode::content(format!("Code block · {language}"), ContentKind::CodeBlock)
                .with_source(element.source.clone());
        node.notes.push(OutlineNote::with_label(
            code.trim_end(),
            format!("Code ({language})"),
            Some(element.source.clone()),
        ));
        node_at_mut(self.document, parent)
            .expect("valid code parent")
            .children
            .push(node);
    }

    fn push_table(&mut self, table: &Element, parent: &[usize]) {
        let rows = descendants(table, "tr")
            .into_iter()
            .map(|row| {
                direct_children(row, &["th", "td"])
                    .into_iter()
                    .map(|cell| element_text(cell, &mut Vec::new()))
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>();
        let mut rows = rows;
        let headers = if rows.is_empty() {
            Vec::new()
        } else {
            rows.remove(0)
        };
        let caption = direct_children(table, &["caption"])
            .first()
            .map(|caption| element_text(caption, &mut Vec::new()))
            .filter(|value| !value.trim().is_empty());
        node_at_mut(self.document, parent)
            .expect("valid table parent")
            .tables
            .push(OutlineTable {
                caption,
                headers,
                rows,
                source: Some(table.source.clone()),
            });
    }

    fn push_image(&mut self, image: &Element, parent: &[usize], source: Option<SourceLocation>) {
        let url = image.attr("src").unwrap_or_default().to_string();
        let alt = image.attr("alt").unwrap_or("Image").trim().to_string();
        let mut node = OutlineNode::content(
            format!("Image: {alt}"),
            ContentKind::Other("image".to_string()),
        )
        .with_source(source.clone().unwrap_or_else(|| image.source.clone()));
        if !url.is_empty() {
            node.links.push(OutlineLink {
                label: Some(alt.clone()),
                target: url.clone(),
                kind: LinkKind::Image,
                source: Some(image.source.clone()),
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
                source: Some(image.source.clone()),
            });
        }
        node_at_mut(self.document, parent)
            .expect("valid image parent")
            .children
            .push(node);
        self.document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            "Image is represented as a reference; no external resource was fetched",
            Some(image.source.clone()),
        ));
    }

    fn append_images(
        &mut self,
        parent: &[usize],
        images: Vec<&Element>,
        source: Option<SourceLocation>,
    ) {
        for image in images {
            self.push_image(image, parent, source.clone());
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

fn task_state(element: &Element) -> Option<TaskState> {
    if let Some(value) = element.attr("data-task") {
        return match value.to_ascii_lowercase().as_str() {
            "done" | "complete" | "completed" | "true" | "checked" => Some(TaskState::Done),
            "todo" | "open" | "false" | "unchecked" => Some(TaskState::Todo),
            _ => None,
        };
    }
    if element.attr("data-checked").is_some() {
        return Some(if element.attr("data-checked") == Some("true") {
            TaskState::Done
        } else {
            TaskState::Todo
        });
    }
    direct_children(element, &["input"])
        .first()
        .filter(|input| input.attr("type") == Some("checkbox"))
        .map(|input| {
            if input.attr("checked").is_some() {
                TaskState::Done
            } else {
                TaskState::Todo
            }
        })
}

fn collect_links(element: &Element) -> Vec<OutlineLink> {
    descendants(element, "a")
        .into_iter()
        .filter_map(|anchor| {
            let href = anchor.attr("href")?;
            Some(OutlineLink {
                label: Some(element_text(anchor, &mut Vec::new())),
                target: href.to_string(),
                kind: LinkKind::Hyperlink,
                source: Some(anchor.source.clone()),
            })
        })
        .collect()
}

fn collect_images(element: &Element) -> Vec<&Element> {
    descendants(element, "img")
}

fn is_ignored_region(name: &str) -> bool {
    matches!(name, "script" | "style" | "nav" | "footer")
}

fn find_element<'a>(nodes: &'a [DomNode], name: &str) -> Option<&'a Element> {
    for node in nodes {
        let DomNode::Element(element) = node else {
            continue;
        };
        if is_ignored_region(&element.name) {
            continue;
        }
        if element.name == name {
            return Some(element);
        }
        if let Some(found) = find_element(&element.children, name) {
            return Some(found);
        }
    }
    None
}

fn descendants<'a>(element: &'a Element, name: &str) -> Vec<&'a Element> {
    let mut output = Vec::new();
    for node in &element.children {
        let DomNode::Element(child) = node else {
            continue;
        };
        if is_ignored_region(&child.name) {
            continue;
        }
        if child.name == name {
            output.push(child);
        }
        output.extend(descendants(child, name));
    }
    output
}

fn direct_children<'a>(element: &'a Element, names: &[&str]) -> Vec<&'a Element> {
    element
        .children
        .iter()
        .filter_map(|node| match node {
            DomNode::Element(child) if names.contains(&child.name.as_str()) => Some(child),
            _ => None,
        })
        .collect()
}

fn element_text(element: &Element, active: &mut Vec<String>) -> String {
    if is_ignored_region(&element.name) || active.iter().any(|name| name == &element.name) {
        return String::new();
    }
    active.push(element.name.clone());
    let mut output = String::new();
    for node in &element.children {
        match node {
            DomNode::Text(text) => output.push_str(text),
            DomNode::Element(child) if child.name == "br" => output.push(' '),
            DomNode::Element(child) => {
                let text = element_text(child, active);
                if !text.is_empty() {
                    if !output.is_empty() && !output.ends_with(char::is_whitespace) {
                        output.push(' ');
                    }
                    output.push_str(&text);
                }
            }
        }
    }
    active.pop();
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn list_item_text(element: &Element) -> String {
    let mut output = String::new();
    for node in &element.children {
        match node {
            DomNode::Text(text) => output.push_str(text),
            DomNode::Element(child) if matches!(child.name.as_str(), "ul" | "ol") => {}
            DomNode::Element(child) if child.name == "br" => output.push(' '),
            DomNode::Element(child) => {
                let text = element_text(child, &mut Vec::new());
                if !text.is_empty() {
                    if !output.is_empty() && !output.ends_with(char::is_whitespace) {
                        output.push(' ');
                    }
                    output.push_str(&text);
                }
            }
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn raw_element_text(element: &Element) -> String {
    let mut output = String::new();
    for node in &element.children {
        match node {
            DomNode::Text(text) => output.push_str(text),
            DomNode::Element(child) => output.push_str(&raw_element_text(child)),
        }
    }
    output
}

#[derive(Clone, Debug)]
struct Element {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<DomNode>,
    source: SourceLocation,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug)]
enum DomNode {
    Element(Element),
    Text(String),
}

#[derive(Debug)]
struct ParsedHtml {
    nodes: Vec<DomNode>,
    warnings: Vec<ConversionWarning>,
}

fn parse_html(input: &str) -> ParsedHtml {
    let mut parser = HtmlParser {
        input,
        offset: 0,
        root: Vec::new(),
        stack: Vec::new(),
        warnings: Vec::new(),
    };
    parser.run();
    ParsedHtml {
        nodes: parser.root,
        warnings: parser.warnings,
    }
}

struct HtmlParser<'a> {
    input: &'a str,
    offset: usize,
    root: Vec<DomNode>,
    stack: Vec<Element>,
    warnings: Vec<ConversionWarning>,
}

impl HtmlParser<'_> {
    fn run(&mut self) {
        while self.offset < self.input.len() {
            if let Some(relative) = self.input[self.offset..].find('<') {
                let tag_start = self.offset + relative;
                if tag_start > self.offset {
                    self.push_text(&self.input[self.offset..tag_start]);
                }
                self.parse_markup(tag_start);
            } else {
                self.push_text(&self.input[self.offset..]);
                self.offset = self.input.len();
            }
        }
        while let Some(element) = self.stack.pop() {
            self.attach(element);
        }
    }

    fn parse_markup(&mut self, start: usize) {
        let remainder = &self.input[start..];
        if remainder.starts_with("<!--") {
            let end = remainder
                .find("-->")
                .map(|index| start + index + 3)
                .unwrap_or(self.input.len());
            self.offset = end;
            return;
        }
        if remainder.starts_with("<!") || remainder.starts_with("<?") {
            let end = remainder
                .find('>')
                .map(|index| start + index + 1)
                .unwrap_or(self.input.len());
            self.offset = end;
            return;
        }
        if let Some(name) = remainder
            .strip_prefix("</")
            .and_then(|value| value.split_once('>'))
            .map(|(name, _)| name.trim().to_ascii_lowercase())
        {
            self.close_element(&name);
            self.offset = start
                + remainder
                    .find('>')
                    .map_or(remainder.len(), |index| index + 1);
            return;
        }
        let Some(relative_end) = find_tag_end(remainder) else {
            self.push_text("<");
            self.offset = start + 1;
            return;
        };
        self.offset = start + relative_end + 1;
        let raw = &remainder[1..relative_end];
        let self_closing = raw.trim_end().ends_with('/');
        let raw = raw.trim_end_matches('/').trim();
        let (name, attributes) = parse_start_tag(raw);
        if name.is_empty() {
            return;
        }
        let source = self.source_location(start);
        let element = Element {
            name: name.clone(),
            attrs: attributes,
            children: Vec::new(),
            source,
        };
        if matches!(name.as_str(), "script" | "style") {
            let close = format!("</{name}");
            let closing = self.input[self.offset..]
                .to_ascii_lowercase()
                .find(&close)
                .map(|index| self.offset + index);
            if let Some(closing) = closing {
                let mut element = element;
                let raw_text = self.input[self.offset..closing].to_string();
                if !raw_text.is_empty() {
                    element.children.push(DomNode::Text(raw_text));
                }
                self.attach(element);
                let end = self.input[closing..]
                    .find('>')
                    .map(|index| closing + index + 1)
                    .unwrap_or(self.input.len());
                self.offset = end;
            } else {
                self.attach(element);
                self.offset = self.input.len();
            }
            return;
        }
        if self_closing || is_html_void(&name) {
            self.attach(element);
        } else {
            self.close_implied(&name);
            self.stack.push(element);
        }
    }

    fn close_implied(&mut self, opening: &str) {
        let index = if matches!(opening, "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            self.stack.iter().rposition(|element| {
                matches!(
                    element.name.as_str(),
                    "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                )
            })
        } else if matches!(
            opening,
            "table" | "pre" | "div" | "section" | "article" | "blockquote"
        ) {
            self.stack.iter().rposition(|element| element.name == "p")
        } else if opening == "li" {
            let list_scope = self
                .stack
                .iter()
                .rposition(|element| matches!(element.name.as_str(), "ul" | "ol"))
                .map_or(0, |index| index + 1);
            self.stack[list_scope..]
                .iter()
                .rposition(|element| element.name == "li")
                .map(|index| list_scope + index)
        } else if opening == "tr" {
            self.stack.iter().rposition(|element| element.name == "tr")
        } else if matches!(opening, "td" | "th") {
            self.stack
                .iter()
                .rposition(|element| matches!(element.name.as_str(), "td" | "th"))
        } else {
            None
        };
        if let Some(index) = index {
            while self.stack.len() > index {
                if let Some(element) = self.stack.pop() {
                    self.attach(element);
                }
            }
        }
    }

    fn close_element(&mut self, name: &str) {
        let Some(index) = self
            .stack
            .iter()
            .rposition(|element| element.name.eq_ignore_ascii_case(name))
        else {
            self.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::InvalidStructure,
                format!("Ignored unmatched closing tag </{name}>"),
                Some(self.source_location(self.offset)),
            ));
            return;
        };
        while self.stack.len() > index {
            if let Some(element) = self.stack.pop() {
                self.attach(element);
            }
        }
    }

    fn push_text(&mut self, raw: &str) {
        let text = decode_entities(raw);
        if text.is_empty() {
            return;
        }
        if let Some(element) = self.stack.last_mut() {
            element.children.push(DomNode::Text(text));
        } else {
            self.root.push(DomNode::Text(text));
        }
    }

    fn attach(&mut self, element: Element) {
        if let Some(parent) = self.stack.last_mut() {
            parent.children.push(DomNode::Element(element));
        } else {
            self.root.push(DomNode::Element(element));
        }
    }

    fn source_location(&self, offset: usize) -> SourceLocation {
        let before = &self.input[..offset.min(self.input.len())];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = before
            .rsplit_once('\n')
            .map_or(before.chars().count() + 1, |(_, tail)| {
                tail.chars().count() + 1
            });
        SourceLocation {
            path: Vec::new(),
            line: Some(line as u32),
            column: Some(column as u32),
            byte_offset: Some(offset),
        }
    }
}

fn find_tag_end(input: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in input.char_indices().skip(1) {
        match (quote, character) {
            (Some(active), current) if active == current => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_start_tag(raw: &str) -> (String, HashMap<String, String>) {
    let name_end = raw.find(char::is_whitespace).unwrap_or(raw.len());
    let name = raw[..name_end].trim().to_ascii_lowercase();
    let mut attrs = HashMap::new();
    let mut remaining = raw[name_end..].trim();
    while !remaining.is_empty() {
        let key_end = remaining
            .find(|character: char| character.is_whitespace() || character == '=')
            .unwrap_or(remaining.len());
        let key = remaining[..key_end].trim().to_ascii_lowercase();
        remaining = remaining[key_end..].trim_start();
        if key.is_empty() {
            let character = remaining.chars().next().expect("non-empty remainder");
            remaining = &remaining[character.len_utf8()..];
            continue;
        }
        let mut value = String::new();
        if let Some(after_equals) = remaining.strip_prefix('=') {
            let after_equals = after_equals.trim_start();
            if let Some(quote) = after_equals
                .chars()
                .next()
                .filter(|value| matches!(value, '"' | '\''))
            {
                let body = &after_equals[quote.len_utf8()..];
                let end = body.find(quote).unwrap_or(body.len());
                value = decode_entities(&body[..end]);
                remaining = if end < body.len() {
                    &body[end + quote.len_utf8()..]
                } else {
                    ""
                };
            } else {
                let end = after_equals
                    .find(char::is_whitespace)
                    .unwrap_or(after_equals.len());
                value = decode_entities(&after_equals[..end]);
                remaining = &after_equals[end..];
            }
        }
        attrs.insert(key, value);
        remaining = remaining.trim_start();
    }
    (name, attrs)
}

fn is_html_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn decode_entities(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(index) = remaining.find('&') {
        output.push_str(&remaining[..index]);
        remaining = &remaining[index..];
        let Some(end) = remaining.find(';') else {
            output.push_str(remaining);
            return output;
        };
        let entity = &remaining[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            value if value.starts_with("#x") || value.starts_with("#X") => {
                u32::from_str_radix(&value[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            value if value.starts_with('#') => {
                value[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            output.push(decoded);
            remaining = &remaining[end + 1..];
        } else {
            output.push('&');
            remaining = &remaining[1..];
        }
    }
    output.push_str(remaining);
    output
}
