//! Platform-neutral view model shared by native Mindmap renderers.
//!
//! XMind remains the source of truth. This module only derives an ordered tree,
//! outline rows, copyable topic JSON, and a deterministic screen layout from the
//! lossless `content.json` document supplied by Rust Core.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::mindmap_ffi::{MindmapGraphStructureDto, MindmapTopicMarkerDto, MindmapTopicStyleDto};

const MIN_NODE_WIDTH: f32 = 168.0;
const MAX_NODE_WIDTH: f32 = 320.0;
const NODE_HEIGHT: f32 = 42.0;
const HORIZONTAL_GAP: f32 = 74.0;
const VERTICAL_GAP: f32 = 18.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapVisualStyle {
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub font_weight: Option<String>,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub text_color: Option<String>,
    pub fill_color: Option<String>,
    pub border_color: Option<String>,
    pub border_width: Option<f64>,
    pub border_pattern: Option<String>,
    pub corner_radius: Option<u16>,
    pub alignment: Option<String>,
}

impl MindmapVisualStyle {
    pub fn from_dto(style: &MindmapTopicStyleDto) -> Self {
        Self {
            font_family: style.font_family.clone(),
            font_size: style.font_size,
            font_weight: style.font_weight.clone(),
            italic: style.italic.unwrap_or(false),
            underline: style.underline.unwrap_or(false),
            strike: style.strike.unwrap_or(false),
            text_color: style.text_color.clone(),
            fill_color: style.node_fill.clone(),
            border_color: style.border_color.clone(),
            border_width: style.border_width,
            border_pattern: style.border_pattern.clone(),
            corner_radius: style.corner_radius,
            alignment: style.alignment.clone(),
        }
    }

    fn from_properties(properties: Option<&serde_json::Map<String, Value>>) -> Self {
        let family = properties
            .and_then(|value| value.get("fo:font-family"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let font_size = properties
            .and_then(|value| value.get("fo:font-size"))
            .and_then(parse_measurement);
        let font_weight = properties
            .and_then(|value| value.get("fo:font-weight"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let font_style = properties
            .and_then(|value| value.get("fo:font-style"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let decoration = properties
            .and_then(|value| value.get("fo:text-decoration"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let text_color = properties
            .and_then(|value| value.get("fo:color"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let fill_color = properties
            .and_then(|value| value.get("svg:fill").or_else(|| value.get("fill-color")))
            .and_then(Value::as_str)
            .filter(|value| !value.eq_ignore_ascii_case("none"))
            .map(ToOwned::to_owned);
        let border_color = properties
            .and_then(|value| value.get("border-line-color"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let border_width = properties
            .and_then(|value| value.get("border-line-width"))
            .and_then(parse_measurement);
        let border_pattern = properties
            .and_then(|value| value.get("border-line-pattern"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let corner_radius = properties
            .and_then(|value| value.get("shape-corner"))
            .and_then(parse_measurement)
            .map(|value| value.clamp(0.0, u16::MAX as f64) as u16);
        let alignment = properties
            .and_then(|value| value.get("fo:text-align"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Self {
            font_family: family,
            font_size,
            font_weight,
            italic: font_style == "italic" || font_style == "oblique",
            underline: decoration
                .split_whitespace()
                .any(|value| value == "underline"),
            strike: decoration
                .split_whitespace()
                .any(|value| value == "line-through"),
            text_color,
            fill_color,
            border_color,
            border_width,
            border_pattern,
            corner_radius,
            alignment,
        }
    }
}

fn parse_measurement(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(value) => {
            let numeric = value
                .chars()
                .skip_while(|character| !character.is_ascii_digit() && *character != '.')
                .collect::<String>();
            let numeric = numeric.trim_end_matches(|character: char| {
                character.is_ascii_alphabetic() || character == '%'
            });
            numeric.parse().ok()
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MindmapMarker {
    pub marker_id: String,
    pub family: String,
    pub label: String,
}

impl MindmapMarker {
    pub fn from_dto(marker: &MindmapTopicMarkerDto) -> Self {
        Self {
            marker_id: marker.marker_id.clone(),
            family: marker.family.clone(),
            label: marker.label.clone(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let marker_id = value.get("markerId")?.as_str()?.to_owned();
        let family = marker_family(&marker_id).to_owned();
        Some(Self {
            marker_id: marker_id.clone(),
            family,
            label: marker_label(&marker_id),
        })
    }

    pub fn is_task(&self) -> bool {
        self.family == "task"
    }
}

fn marker_family(marker_id: &str) -> &'static str {
    if marker_id.starts_with("priority-") {
        "priority"
    } else if marker_id.starts_with("task-") {
        "task"
    } else if marker_id.starts_with("tag-") {
        "tag"
    } else if marker_id.starts_with("flag-") {
        "flag"
    } else if marker_id.starts_with("star-") {
        "star"
    } else if marker_id.starts_with("people-") {
        "people"
    } else if marker_id.starts_with("smiley-") {
        "smiley"
    } else if marker_id.starts_with("symbol-") || marker_id.starts_with("symbol_") {
        "symbol"
    } else {
        "other"
    }
}

fn marker_label(marker_id: &str) -> String {
    if let Some(level) = marker_id.strip_prefix("priority-") {
        return format!("P{level}");
    }
    match marker_id {
        "task-start" => "○".to_string(),
        "task-oct" => "12%".to_string(),
        "task-quarter" => "25%".to_string(),
        "task-3oct" => "38%".to_string(),
        "task-half" => "50%".to_string(),
        "task-5oct" => "62%".to_string(),
        "task-3quar" => "75%".to_string(),
        "task-7oct" => "88%".to_string(),
        "task-done" => "✓".to_string(),
        "task-pause" => "Ⅱ".to_string(),
        value if value.starts_with("flag-") => "⚑".to_string(),
        value if value.starts_with("star-") => "★".to_string(),
        value if value.starts_with("tag-") => "#".to_string(),
        value if value.starts_with("people-") => "●".to_string(),
        value if value.starts_with("smiley-") => "☺".to_string(),
        value if value.starts_with("symbol-") || value.starts_with("symbol_") => "!".to_string(),
        value => value
            .rsplit('-')
            .next()
            .unwrap_or(value)
            .chars()
            .take(2)
            .collect::<String>()
            .to_uppercase(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MindmapTopic {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub child_ids: Vec<String>,
    pub style: MindmapVisualStyle,
    pub shape_class: Option<String>,
    pub markers: Vec<MindmapMarker>,
    pub is_complete: bool,
}

impl MindmapTopic {
    pub fn title_draft(&self) -> String {
        self.title.clone()
    }

    pub fn marker_summary(&self) -> String {
        self.markers
            .iter()
            .filter(|marker| !marker.is_task() || marker.marker_id == "task-done")
            .map(|marker| marker.label.clone())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapTree {
    pub topics: BTreeMap<String, MindmapTopic>,
    pub root_id: Option<String>,
}

impl MindmapTree {
    pub fn from_content_json(content_json: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(content_json)
            .map_err(|error| format!("content.json 不是有效 JSON：{error}"))?;
        let sheets = root
            .as_array()
            .ok_or_else(|| "content.json 根节点必须是数组".to_string())?;
        let root_topic = sheets
            .first()
            .and_then(|sheet| sheet.get("rootTopic"))
            .ok_or_else(|| "content.json 不包含 rootTopic".to_string())?;
        let mut tree = Self::default();
        tree.walk_topic(root_topic, None, 0)?;
        if tree.root_id.is_none() {
            return Err("rootTopic 无效".to_string());
        }
        Ok(tree)
    }

    fn walk_topic(
        &mut self,
        value: &Value,
        parent_id: Option<&str>,
        depth: usize,
    ) -> Result<(), String> {
        let object = value
            .as_object()
            .ok_or_else(|| "topic 必须是对象".to_string())?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "topic 缺少 id".to_string())?
            .to_owned();
        if self.topics.contains_key(&id) {
            return Err(format!("topic id 重复：{id}"));
        }
        if depth == 0 {
            self.root_id = Some(id.clone());
        }
        let title = object
            .get("title")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("未命名主题")
            .to_owned();
        let properties = object
            .get("style")
            .and_then(Value::as_object)
            .and_then(|style| style.get("properties"))
            .and_then(Value::as_object);
        let style = MindmapVisualStyle::from_properties(properties);
        let shape_class = object
            .get("shape-class")
            .and_then(Value::as_str)
            .or_else(|| {
                properties
                    .and_then(|value| value.get("shape-class"))
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned);
        let markers = object
            .get("markers")
            .and_then(Value::as_array)
            .map(|markers| {
                markers
                    .iter()
                    .filter_map(MindmapMarker::from_json)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let child_values = object
            .get("children")
            .and_then(Value::as_object)
            .and_then(|children| children.get("attached"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let child_ids = child_values
            .iter()
            .filter_map(|child| child.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let is_complete = markers.iter().any(|marker| marker.marker_id == "task-done");
        self.topics.insert(
            id.clone(),
            MindmapTopic {
                id: id.clone(),
                title,
                parent_id: parent_id.map(ToOwned::to_owned),
                depth,
                child_ids,
                style,
                shape_class,
                markers,
                is_complete,
            },
        );
        for child in child_values {
            self.walk_topic(&child, Some(&id), depth + 1)?;
        }
        Ok(())
    }

    pub fn selected(&self, id: Option<&str>) -> Option<&MindmapTopic> {
        id.and_then(|id| self.topics.get(id))
    }

    pub fn outline_rows(
        &self,
        query: &str,
        collapsed: &std::collections::BTreeSet<String>,
    ) -> Vec<MindmapOutlineRow> {
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            return self
                .topics
                .values()
                .filter(|topic| {
                    topic.title.to_lowercase().contains(&query)
                        || topic.id.to_lowercase().contains(&query)
                })
                .take(80)
                .map(MindmapOutlineRow::from_topic)
                .collect();
        }

        let Some(root_id) = self.root_id.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        self.append_outline(root_id, collapsed, &mut rows);
        rows
    }

    fn append_outline(
        &self,
        id: &str,
        collapsed: &std::collections::BTreeSet<String>,
        rows: &mut Vec<MindmapOutlineRow>,
    ) {
        let Some(topic) = self.topics.get(id) else {
            return;
        };
        rows.push(MindmapOutlineRow::from_topic(topic));
        if collapsed.contains(id) {
            return;
        }
        for child_id in &topic.child_ids {
            self.append_outline(child_id, collapsed, rows);
        }
    }

    pub fn visible_topics<'a>(
        &'a self,
        collapsed: &std::collections::BTreeSet<String>,
    ) -> Vec<&'a MindmapTopic> {
        let Some(root_id) = self.root_id.as_deref() else {
            return Vec::new();
        };
        let mut topics = Vec::new();
        self.append_visible(root_id, collapsed, &mut topics);
        topics
    }

    fn append_visible<'a>(
        &'a self,
        id: &str,
        collapsed: &std::collections::BTreeSet<String>,
        topics: &mut Vec<&'a MindmapTopic>,
    ) {
        let Some(topic) = self.topics.get(id) else {
            return;
        };
        topics.push(topic);
        if collapsed.contains(id) {
            return;
        }
        for child_id in &topic.child_ids {
            self.append_visible(child_id, collapsed, topics);
        }
    }

    pub fn layout(
        &self,
        collapsed: &std::collections::BTreeSet<String>,
        structure: MindmapGraphStructureDto,
        zoom: f32,
    ) -> MindmapLayout {
        let Some(root_id) = self.root_id.as_deref() else {
            return MindmapLayout::default();
        };
        let mut engine = LayoutEngine::new(self, collapsed, zoom);
        let _ = match structure {
            MindmapGraphStructureDto::MindMap => engine.layout_mindmap(root_id),
            MindmapGraphStructureDto::OrgChart => engine.layout_org_chart(root_id),
            MindmapGraphStructureDto::LogicLeft => engine.layout_tree(root_id, -1.0),
            MindmapGraphStructureDto::LogicRight
            | MindmapGraphStructureDto::Tree
            | MindmapGraphStructureDto::Unknown => engine.layout_tree(root_id, 1.0),
        };
        engine.finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MindmapOutlineRow {
    pub id: String,
    pub title: String,
    pub depth: usize,
    pub child_count: usize,
    pub is_collapsed: bool,
    pub marker_summary: String,
    pub is_complete: bool,
}

impl MindmapOutlineRow {
    fn from_topic(topic: &MindmapTopic) -> Self {
        Self {
            id: topic.id.clone(),
            title: topic.title.clone(),
            depth: topic.depth,
            child_count: topic.child_ids.len(),
            is_collapsed: false,
            marker_summary: topic.marker_summary(),
            is_complete: topic.is_complete,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapNodeLayout {
    pub id: String,
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub parent_id: Option<String>,
    pub depth: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapLayout {
    pub nodes: Vec<MindmapNodeLayout>,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
    pub width: f32,
    pub height: f32,
}

impl MindmapLayout {
    pub fn node(&self, id: &str) -> Option<&MindmapNodeLayout> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

struct LayoutEngine<'a> {
    tree: &'a MindmapTree,
    collapsed: &'a std::collections::BTreeSet<String>,
    zoom: f32,
    nodes: BTreeMap<String, MindmapNodeLayout>,
    next_leaf: f32,
}

impl<'a> LayoutEngine<'a> {
    fn new(
        tree: &'a MindmapTree,
        collapsed: &'a std::collections::BTreeSet<String>,
        zoom: f32,
    ) -> Self {
        Self {
            tree,
            collapsed,
            zoom: zoom.clamp(0.25, 2.5),
            nodes: BTreeMap::new(),
            next_leaf: 0.0,
        }
    }

    fn node_width(&self, topic: &MindmapTopic) -> f32 {
        let chars = topic.title.chars().count().min(26) as f32;
        ((MIN_NODE_WIDTH + chars * 5.6) * self.zoom)
            .clamp(MIN_NODE_WIDTH * self.zoom, MAX_NODE_WIDTH * self.zoom)
    }

    fn node_height(&self) -> f32 {
        NODE_HEIGHT * self.zoom
    }

    fn child_y_gap(&self) -> f32 {
        (NODE_HEIGHT + VERTICAL_GAP) * self.zoom
    }

    fn horizontal_step(&self) -> f32 {
        (MAX_NODE_WIDTH + HORIZONTAL_GAP) * self.zoom * 0.78
    }

    fn layout_tree(&mut self, root_id: &str, direction: f32) -> f32 {
        self.layout_vertical_branch(root_id, 0, direction)
    }

    fn layout_vertical_branch(&mut self, id: &str, depth: usize, direction: f32) -> f32 {
        let Some(topic) = self.tree.topics.get(id) else {
            return 0.0;
        };
        let children = if self.collapsed.contains(id) {
            &[][..]
        } else {
            topic.child_ids.as_slice()
        };
        let y = if children.is_empty() {
            let y = self.next_leaf;
            self.next_leaf += self.child_y_gap();
            y
        } else {
            let child_centers = children
                .iter()
                .map(|child| self.layout_vertical_branch(child, depth + 1, direction))
                .collect::<Vec<_>>();
            child_centers.iter().sum::<f32>() / child_centers.len().max(1) as f32
        };
        self.nodes.insert(
            id.to_owned(),
            MindmapNodeLayout {
                id: id.to_owned(),
                center_x: direction * depth as f32 * self.horizontal_step(),
                center_y: y,
                width: self.node_width(topic),
                height: self.node_height(),
                parent_id: topic.parent_id.clone(),
                depth,
            },
        );
        y
    }

    fn layout_mindmap(&mut self, root_id: &str) -> f32 {
        let Some(root) = self.tree.topics.get(root_id) else {
            return 0.0;
        };
        let children = if self.collapsed.contains(root_id) {
            Vec::new()
        } else {
            root.child_ids.clone()
        };
        let split = children.len().div_ceil(2);
        let mut right_centers = Vec::new();
        for child in &children[..split] {
            self.next_leaf = 0.0;
            right_centers.push(self.layout_mindmap_branch(child, 1, 1.0));
        }
        let right_top = right_centers.iter().copied().fold(f32::INFINITY, f32::min);
        let right_bottom = right_centers
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let mut left_centers = Vec::new();
        for child in &children[split..] {
            self.next_leaf = 0.0;
            left_centers.push(self.layout_mindmap_branch(child, 1, -1.0));
        }
        let left_top = left_centers.iter().copied().fold(f32::INFINITY, f32::min);
        let left_bottom = left_centers
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let y = if left_centers.is_empty() && right_centers.is_empty() {
            0.0
        } else if left_centers.is_empty() {
            (right_top + right_bottom) / 2.0
        } else if right_centers.is_empty() {
            (left_top + left_bottom) / 2.0
        } else {
            let right_mid = (right_top + right_bottom) / 2.0;
            let left_mid = (left_top + left_bottom) / 2.0;
            (right_mid + left_mid) / 2.0
        };
        self.nodes.insert(
            root_id.to_owned(),
            MindmapNodeLayout {
                id: root_id.to_owned(),
                center_x: 0.0,
                center_y: y,
                width: self.node_width(root),
                height: self.node_height(),
                parent_id: None,
                depth: 0,
            },
        );
        y
    }

    fn layout_mindmap_branch(&mut self, id: &str, depth: usize, direction: f32) -> f32 {
        let Some(topic) = self.tree.topics.get(id) else {
            return 0.0;
        };
        let children = if self.collapsed.contains(id) {
            &[][..]
        } else {
            topic.child_ids.as_slice()
        };
        let y = if children.is_empty() {
            let y = self.next_leaf;
            self.next_leaf += self.child_y_gap();
            y
        } else {
            let child_centers = children
                .iter()
                .map(|child| self.layout_mindmap_branch(child, depth + 1, direction))
                .collect::<Vec<_>>();
            child_centers.iter().sum::<f32>() / child_centers.len().max(1) as f32
        };
        self.nodes.insert(
            id.to_owned(),
            MindmapNodeLayout {
                id: id.to_owned(),
                center_x: direction * depth as f32 * self.horizontal_step(),
                center_y: y,
                width: self.node_width(topic),
                height: self.node_height(),
                parent_id: topic.parent_id.clone(),
                depth,
            },
        );
        y
    }

    fn layout_org_chart(&mut self, root_id: &str) -> f32 {
        self.layout_org_branch(root_id, 0)
    }

    fn layout_org_branch(&mut self, id: &str, depth: usize) -> f32 {
        let Some(topic) = self.tree.topics.get(id) else {
            return 0.0;
        };
        let children = if self.collapsed.contains(id) {
            &[][..]
        } else {
            topic.child_ids.as_slice()
        };
        let center_x = if children.is_empty() {
            let x = self.next_leaf;
            self.next_leaf += self.node_width(topic) + 28.0 * self.zoom;
            x
        } else {
            let child_centers = children
                .iter()
                .map(|child| self.layout_org_branch(child, depth + 1))
                .collect::<Vec<_>>();
            child_centers.iter().sum::<f32>() / child_centers.len().max(1) as f32
        };
        let center_y = depth as f32 * (NODE_HEIGHT + 76.0) * self.zoom;
        self.nodes.insert(
            id.to_owned(),
            MindmapNodeLayout {
                id: id.to_owned(),
                center_x,
                center_y,
                width: self.node_width(topic),
                height: self.node_height(),
                parent_id: topic.parent_id.clone(),
                depth,
            },
        );
        center_x
    }

    fn finish(self) -> MindmapLayout {
        let mut nodes = self.nodes.into_values().collect::<Vec<_>>();
        nodes.sort_by(|left, right| {
            left.depth
                .cmp(&right.depth)
                .then_with(|| left.center_y.total_cmp(&right.center_y))
                .then_with(|| left.center_x.total_cmp(&right.center_x))
                .then_with(|| left.id.cmp(&right.id))
        });
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for node in &nodes {
            min_x = min_x.min(node.center_x - node.width / 2.0);
            max_x = max_x.max(node.center_x + node.width / 2.0);
            min_y = min_y.min(node.center_y - node.height / 2.0);
            max_y = max_y.max(node.center_y + node.height / 2.0);
        }
        if nodes.is_empty() {
            min_x = 0.0;
            min_y = 0.0;
            max_x = 0.0;
            max_y = 0.0;
        }
        MindmapLayout {
            nodes,
            min_x,
            min_y,
            max_x,
            max_y,
            width: (max_x - min_x).max(0.0),
            height: (max_y - min_y).max(0.0),
        }
    }
}

pub fn topic_json(content_json: &str, topic_id: &str) -> Option<String> {
    let root: Value = serde_json::from_str(content_json).ok()?;
    fn find(value: &Value, topic_id: &str) -> Option<Value> {
        if value.get("id").and_then(Value::as_str) == Some(topic_id) {
            return Some(value.clone());
        }
        if let Some(object) = value.as_object() {
            for child in object.values() {
                if let Some(found) = find(child, topic_id) {
                    return Some(found);
                }
            }
        }
        if let Some(values) = value.as_array() {
            for child in values {
                if let Some(found) = find(child, topic_id) {
                    return Some(found);
                }
            }
        }
        None
    }
    serde_json::to_string(&find(&root, topic_id)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT: &str = r#"[
      {
        "id": "sheet-1",
        "rootTopic": {
          "id": "root",
          "title": "Root",
          "children": {
            "attached": [
              {
                "id": "a",
                "title": "Alpha",
                "markers": [{"markerId": "priority-2"}],
                "children": {"attached": [{"id": "a1", "title": "Alpha child"}]}
              },
              {
                "id": "b",
                "title": "Beta",
                "markers": [{"markerId": "task-done"}]
              }
            ]
          }
        }
      }
    ]"#;

    #[test]
    fn parses_tree_markers_and_copyable_topic_json() {
        let tree = MindmapTree::from_content_json(CONTENT).unwrap();
        assert_eq!(tree.root_id.as_deref(), Some("root"));
        assert_eq!(tree.topics.len(), 4);
        assert_eq!(tree.topics["a"].child_ids, vec!["a1".to_string()]);
        assert_eq!(tree.topics["a"].markers[0].label, "P2");
        assert!(tree.topics["b"].is_complete);
        let copied = topic_json(CONTENT, "a").unwrap();
        assert!(copied.contains("\"a1\""));
    }

    #[test]
    fn outline_respects_collapse_and_search() {
        let tree = MindmapTree::from_content_json(CONTENT).unwrap();
        let mut collapsed = std::collections::BTreeSet::new();
        assert_eq!(tree.outline_rows("", &collapsed).len(), 4);
        collapsed.insert("a".to_string());
        let rows = tree.outline_rows("", &collapsed);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.id != "a1"));
        let matches = tree.outline_rows("child", &collapsed);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "a1");
    }

    #[test]
    fn layouts_cover_every_visible_topic_without_duplicate_nodes() {
        let tree = MindmapTree::from_content_json(CONTENT).unwrap();
        let collapsed = std::collections::BTreeSet::new();
        for structure in [
            MindmapGraphStructureDto::MindMap,
            MindmapGraphStructureDto::LogicRight,
            MindmapGraphStructureDto::LogicLeft,
            MindmapGraphStructureDto::OrgChart,
            MindmapGraphStructureDto::Tree,
        ] {
            let layout = tree.layout(&collapsed, structure, 0.82);
            assert_eq!(layout.nodes.len(), 4);
            let ids = layout
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(ids.len(), layout.nodes.len());
            assert!(layout.width > 0.0);
            assert!(layout.height > 0.0);
        }
    }

    #[test]
    fn zoom_changes_layout_geometry() {
        let tree = MindmapTree::from_content_json(CONTENT).unwrap();
        let collapsed = std::collections::BTreeSet::new();
        let small = tree.layout(&collapsed, MindmapGraphStructureDto::LogicRight, 0.5);
        let large = tree.layout(&collapsed, MindmapGraphStructureDto::LogicRight, 1.5);
        assert!(large.width > small.width);
        assert!(large.height > small.height);
    }
}
