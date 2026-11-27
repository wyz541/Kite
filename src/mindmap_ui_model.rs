//! Platform-neutral view model shared by native Mindmap renderers.
//!
//! XMind remains the source of truth. This module only derives an ordered tree,
//! outline rows, copyable topic JSON, and a deterministic screen layout from the
//! lossless `content.json` document supplied by Rust Core.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::mindmap_ffi::{
    MindmapDatePartOrderDto, MindmapGraphStructureDto, MindmapTopicMarkerDto, MindmapTopicStyleDto,
};

const MIN_NODE_WIDTH: f32 = 168.0;
const MAX_NODE_WIDTH: f32 = 320.0;
const NODE_HEIGHT: f32 = 42.0;
const HORIZONTAL_GAP: f32 = 74.0;
const VERTICAL_GAP: f32 = 18.0;
const VISIBLE_MARKER_LIMIT: usize = 8;

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
    pub line_height: Option<String>,
    pub letter_spacing: Option<String>,
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
            line_height: style.line_height.clone(),
            letter_spacing: style.letter_spacing.clone(),
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
        let line_height = properties
            .and_then(|value| value.get("fo:line-height"))
            .and_then(xmind_string);
        let letter_spacing = properties
            .and_then(|value| value.get("fo:letter-spacing"))
            .and_then(xmind_string);
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
            line_height,
            letter_spacing,
        }
    }
}

fn xmind_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Parse the numeric portion of an XMind measurement while preserving the raw
/// value in the DTO for lossless writes.
pub fn measurement_number(value: Option<&str>) -> Option<f64> {
    let value = value?.trim();
    let numeric = value
        .trim_end_matches(|character: char| character.is_ascii_alphabetic() || character == '%')
        .trim();
    numeric.parse().ok()
}

/// GPUI does not expose CSS-like letter spacing. This produces a visual-only
/// approximation with thin spaces while the persisted title remains unchanged.
pub fn letter_spacing_preview(text: &str, spacing: Option<f64>) -> String {
    let Some(spacing) = spacing.filter(|value| *value > 0.05) else {
        return text.to_owned();
    };
    let spacer = if spacing >= 0.35 {
        "\u{2009}\u{2009}"
    } else {
        "\u{2009}"
    };
    let mut output = String::with_capacity(text.len() + text.chars().count() * 2);
    for (index, character) in text.chars().enumerate() {
        if index > 0 {
            output.push_str(spacer);
        }
        output.push(character);
    }
    output
}

/// Resolve the host's short-date order into a stable request for Core.
pub fn system_date_pattern() -> (MindmapDatePartOrderDto, String) {
    #[cfg(target_os = "windows")]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;

        if let Ok(international) =
            RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Control Panel\International")
        {
            let short_date = international
                .get_value::<String, _>("sShortDate")
                .unwrap_or_default();
            let separator = international
                .get_value::<String, _>("sDate")
                .unwrap_or_else(|_| "/".to_string());
            return parse_system_date_pattern(&short_date, &separator);
        }
    }
    (MindmapDatePartOrderDto::YearMonthDay, "/".to_string())
}

/// Pure parser used by the Windows locale reader and regression tests.
pub fn parse_system_date_pattern(
    short_date: &str,
    separator_hint: &str,
) -> (MindmapDatePartOrderDto, String) {
    let lower = short_date.to_ascii_lowercase();
    let year = lower.find('y');
    let month = lower.find('m');
    let day = lower.find('d');
    let order = match (year, month, day) {
        (Some(year), Some(month), Some(day)) if year < month && month < day => {
            MindmapDatePartOrderDto::YearMonthDay
        }
        (Some(year), Some(month), Some(day)) if month < day && day < year => {
            MindmapDatePartOrderDto::MonthDayYear
        }
        (Some(year), Some(month), Some(day)) if day < month && month < year => {
            MindmapDatePartOrderDto::DayMonthYear
        }
        _ => MindmapDatePartOrderDto::YearMonthDay,
    };
    let separator = separator_hint
        .trim()
        .chars()
        .find(|character| !character.is_ascii_alphanumeric() && !character.is_whitespace())
        .unwrap_or('/')
        .to_string();
    (order, separator)
}

/// Installed font families, cached for the lifetime of the process. The raw
/// requested family remains in the document; this list is only for rendering
/// fallback and the picker.
pub fn system_font_families() -> Vec<String> {
    static FONTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut database = fontdb::Database::new();
            database.load_system_fonts();
            let mut families = database
                .faces()
                .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
                .collect::<Vec<_>>();
            families.sort_by_key(|family| family.to_ascii_lowercase());
            families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            if families.is_empty() {
                families = vec![
                    "Segoe UI".to_string(),
                    "Arial".to_string(),
                    "Calibri".to_string(),
                    "Consolas".to_string(),
                ];
            }
            families
        })
        .clone()
}

pub fn missing_font_family(requested: Option<&str>, available: &[String]) -> Option<String> {
    let requested = requested?.trim();
    if requested.is_empty() {
        return None;
    }
    let generic = [
        "serif",
        "sans-serif",
        "monospace",
        "cursive",
        "fantasy",
        "system-ui",
    ];
    let found = requested.split(',').map(str::trim).any(|candidate| {
        generic
            .iter()
            .any(|name| candidate.eq_ignore_ascii_case(name))
            || available
                .iter()
                .any(|family| family.eq_ignore_ascii_case(candidate))
    });
    (!found).then(|| requested.to_string())
}

pub fn resolve_render_font(
    requested: Option<&str>,
    available: &[String],
    fallback: &str,
) -> String {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback.to_string();
    };
    for candidate in requested.split(',').map(str::trim) {
        if [
            "serif",
            "sans-serif",
            "monospace",
            "cursive",
            "fantasy",
            "system-ui",
        ]
        .iter()
        .any(|name| candidate.eq_ignore_ascii_case(name))
            || available
                .iter()
                .any(|family| family.eq_ignore_ascii_case(candidate))
        {
            return candidate.to_string();
        }
    }
    fallback.to_string()
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
        let marker_id = value
            .get("markerId")?
            .as_str()
            .filter(|value| !value.is_empty())?
            .to_owned();
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
        "symbol-wrong" => "⛔".to_string(),
        "symbol-attention" => "⚠".to_string(),
        "symbol-pause" => "Ⅱ".to_string(),
        "symbol-ok" => "✓".to_string(),
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
    pub opaque_marker_count: usize,
    pub is_complete: bool,
    /// Flat read-only convenience fields consumed by the Windows inspector.
    pub task_presentation: Option<String>,
    pub todo_status: Option<String>,
    pub todo_progress: Option<f64>,
    /// Full normalized task semantics shared by the Windows and macOS parity
    /// projections.
    pub task: Option<MindmapTaskSemantics>,
}

/// XMind 27.1 task fields retained verbatim for canonical UI parity checks.
///
/// Modern top-level task fields take precedence over legacy `task-*` marker
/// fallbacks. Unknown or conflicting values are normalized without rewriting the
/// source document so both native renderers make the same decision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapTaskSemantics {
    pub task_presentation: Option<String>,
    pub todo_status: Option<String>,
    pub todo_progress: Option<f64>,
    pub task_progress: Option<f64>,
    pub task_start_date: Option<String>,
    pub task_due_date: Option<String>,
    pub legacy_status: Option<String>,
    pub legacy_progress: Option<f64>,
    pub effective_status: Option<String>,
    pub effective_progress: Option<f64>,
    pub source: Option<String>,
    pub has_conflict: bool,
}

impl MindmapTaskSemantics {
    fn from_topic(object: &Map<String, Value>, markers: &[MindmapMarker]) -> Option<Self> {
        let task_presentation = string_field(object, "taskPresentation");
        let todo_status = string_field(object, "todoStatus");
        let todo_progress = number_field(object, "todoProgress");
        let task_progress = number_field(object, "taskProgress");
        let task_start_date = string_field(object, "taskStartDate");
        let task_due_date = string_field(object, "taskDueDate");

        let (legacy_status, legacy_progress) = markers
            .iter()
            .find(|marker| marker.marker_id.starts_with("task-"))
            .map(|marker| task_marker_state(&marker.marker_id))
            .unwrap_or((None, None));

        let modern_present = task_presentation.is_some()
            || todo_status.is_some()
            || todo_progress.is_some()
            || task_progress.is_some()
            || task_start_date.is_some()
            || task_due_date.is_some();
        let modern_progress = todo_progress.or(task_progress).map(normalize_progress);
        let modern_status = todo_status
            .as_deref()
            .and_then(normalize_task_status)
            .map(ToOwned::to_owned)
            .or_else(|| modern_progress.and_then(status_from_progress))
            .or_else(|| modern_present.then(|| "not-started".to_owned()));

        if !modern_present && legacy_status.is_none() && legacy_progress.is_none() {
            return None;
        }

        let effective_status = modern_status.clone().or_else(|| legacy_status.clone());
        let effective_progress = modern_progress.or(legacy_progress);
        let status_conflict = modern_status
            .as_deref()
            .zip(legacy_status.as_deref())
            .is_some_and(|(modern, legacy)| modern != legacy);
        let progress_conflict = modern_progress
            .zip(legacy_progress)
            .is_some_and(|(modern, legacy)| (modern - legacy).abs() > 0.000_001);
        let source = match (
            modern_present,
            legacy_status.is_some() || legacy_progress.is_some(),
        ) {
            (true, true) => "modern+legacy",
            (true, false) => "modern",
            (false, true) => "legacy",
            (false, false) => unreachable!(),
        };

        Some(Self {
            task_presentation,
            todo_status,
            todo_progress: todo_progress.map(normalize_progress),
            task_progress: task_progress.map(normalize_progress),
            task_start_date,
            task_due_date,
            legacy_status,
            legacy_progress,
            effective_status,
            effective_progress,
            source: Some(source.to_owned()),
            has_conflict: status_conflict || progress_conflict,
        })
    }

    fn to_canonical_json(&self) -> Value {
        canonical_object(vec![
            (
                "effectiveProgress",
                optional_number(self.effective_progress),
            ),
            ("effectiveStatus", optional_string(&self.effective_status)),
            ("hasConflict", Value::Bool(self.has_conflict)),
            ("legacyProgress", optional_number(self.legacy_progress)),
            ("legacyStatus", optional_string(&self.legacy_status)),
            ("source", optional_string(&self.source)),
            ("taskDueDate", optional_string(&self.task_due_date)),
            ("taskPresentation", optional_string(&self.task_presentation)),
            ("taskProgress", optional_number(self.task_progress)),
            ("taskStartDate", optional_string(&self.task_start_date)),
            ("todoProgress", optional_number(self.todo_progress)),
            ("todoStatus", optional_string(&self.todo_status)),
        ])
    }
}

impl MindmapTopic {
    pub fn title_draft(&self) -> String {
        self.title.clone()
    }

    pub fn marker_summary(&self) -> String {
        let mut visible = self
            .markers
            .iter()
            .filter(|marker| {
                !marker.is_task() || matches!(marker.marker_id.as_str(), "task-done" | "task-pause")
            })
            .map(|marker| marker.label.clone())
            .collect::<Vec<_>>();
        if self.is_complete && !visible.iter().any(|label| label == "✓") {
            visible.insert(0, "✓".to_string());
        }
        let total = visible.len();
        let mut summary = visible
            .into_iter()
            .take(VISIBLE_MARKER_LIMIT)
            .collect::<Vec<_>>()
            .join(" ");
        if total > VISIBLE_MARKER_LIMIT {
            summary.push_str(&format!(" +{}", total - VISIBLE_MARKER_LIMIT));
        }
        summary
    }

    fn to_canonical_json(&self) -> Value {
        let marker_values = self
            .markers
            .iter()
            .map(|marker| {
                canonical_object(vec![
                    ("family", Value::String(marker.family.clone())),
                    ("id", Value::String(marker.marker_id.clone())),
                    ("label", Value::String(marker.label.clone())),
                    ("opaque", Value::Bool(marker.family == "other")),
                ])
            })
            .collect::<Vec<_>>();
        canonical_object(vec![
            (
                "children",
                Value::Array(
                    self.child_ids
                        .iter()
                        .map(|id| Value::String(id.clone()))
                        .collect(),
                ),
            ),
            ("depth", json!(self.depth)),
            ("id", Value::String(self.id.clone())),
            ("markers", Value::Array(marker_values)),
            ("opaque_marker_count", json!(self.opaque_marker_count)),
            ("parent_id", optional_string(&self.parent_id)),
            ("shape_class", optional_string(&self.shape_class)),
            ("style", self.style.to_canonical_json()),
            (
                "task",
                self.task
                    .as_ref()
                    .map_or(Value::Null, MindmapTaskSemantics::to_canonical_json),
            ),
            ("title", Value::String(self.title.clone())),
        ])
    }
}

impl MindmapVisualStyle {
    fn to_canonical_json(&self) -> Value {
        canonical_object(vec![
            ("alignment", optional_string(&self.alignment)),
            ("border_color", optional_string(&self.border_color)),
            ("border_pattern", optional_string(&self.border_pattern)),
            (
                "border_width",
                optional_number(Some(self.border_width.unwrap_or(1.0))),
            ),
            ("corner_radius", json!(self.corner_radius)),
            ("fill_color", optional_string(&self.fill_color)),
            ("font_family", optional_string(&self.font_family)),
            (
                "font_size",
                optional_number(Some(self.font_size.unwrap_or(12.5))),
            ),
            ("font_weight", optional_string(&self.font_weight)),
            ("italic", Value::Bool(self.italic)),
            ("strike", Value::Bool(self.strike)),
            ("text_color", optional_string(&self.text_color)),
            ("underline", Value::Bool(self.underline)),
        ])
    }
}

impl MindmapCanonicalConnector {
    fn to_canonical_json(&self) -> Value {
        canonical_object(vec![
            ("color", optional_string(&self.color)),
            ("corner_radius", optional_string(&self.corner_radius)),
            ("kind", optional_string(&self.kind)),
            ("pattern", optional_string(&self.pattern)),
            ("width", optional_string(&self.width)),
        ])
    }
}

fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn number_field(object: &Map<String, Value>, key: &str) -> Option<f64> {
    let value = object.get(key)?;
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(value) => parse_percent_or_number(value),
        _ => None,
    }
}

fn parse_percent_or_number(value: &str) -> Option<f64> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f64>()
            .ok()
            .map(|value| value / 100.0);
    }
    value.parse().ok()
}

fn normalize_progress(value: f64) -> f64 {
    let value = if value > 1.0 && value <= 100.0 {
        value / 100.0
    } else {
        value
    };
    value.clamp(0.0, 1.0)
}

fn normalize_task_status(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "todo" | "not-started" | "not_started" | "notstarted" | "open" => Some("not-started"),
        "in-progress" | "in_progress" | "inprogress" | "progress" | "started" => {
            Some("in-progress")
        }
        "paused" | "blocked" | "on-hold" | "on_hold" => Some("paused"),
        "done" | "completed" | "complete" => Some("done"),
        _ => None,
    }
}

fn status_from_progress(progress: f64) -> Option<String> {
    let status = if progress >= 1.0 {
        "done"
    } else if progress > 0.0 {
        "in-progress"
    } else {
        "not-started"
    };
    Some(status.to_owned())
}

fn task_marker_state(marker_id: &str) -> (Option<String>, Option<f64>) {
    let progress = match marker_id {
        "task-start" => Some(0.0),
        "task-oct" => Some(1.0 / 8.0),
        "task-quarter" => Some(2.0 / 8.0),
        "task-3oct" => Some(3.0 / 8.0),
        "task-half" => Some(4.0 / 8.0),
        "task-5oct" => Some(5.0 / 8.0),
        "task-3quar" => Some(6.0 / 8.0),
        "task-7oct" => Some(7.0 / 8.0),
        "task-done" => Some(1.0),
        "task-pause" => None,
        _ => None,
    };
    let status = if marker_id == "task-pause" {
        "paused"
    } else if progress.is_some_and(|progress| progress >= 1.0) {
        "done"
    } else if progress.is_some_and(|progress| progress > 0.0) {
        "in-progress"
    } else if progress.is_some() {
        "not-started"
    } else {
        "in-progress"
    };
    (Some(status.to_owned()), progress)
}

fn optional_string(value: &Option<String>) -> Value {
    value
        .as_ref()
        .map_or(Value::Null, |value| Value::String(value.clone()))
}

fn optional_number(value: Option<f64>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    if value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64
    {
        return json!(value as i64);
    }
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

fn canonical_object(entries: Vec<(&str, Value)>) -> Value {
    let mut entries = entries;
    entries.sort_by_key(|(key, _)| *key);
    let mut object = Map::new();
    for (key, value) in entries {
        object.insert(key.to_owned(), value);
    }
    Value::Object(object)
}

fn structure_name(value: Option<&str>) -> String {
    match value {
        Some("org.xmind.ui.logic.right" | "org.xmind.ui.logic-chart.right") => "logic-right",
        Some("org.xmind.ui.logic.left" | "org.xmind.ui.logic-chart.left") => "logic-left",
        Some("org.xmind.ui.org-chart.down") => "org-chart",
        Some("org.xmind.ui.tree.right") => "tree",
        Some(value) if value.starts_with("org.xmind.ui.map") => "mind-map",
        Some(_) => "unknown",
        None => "unknown",
    }
    .to_owned()
}

fn connector_from_sheet(sheet: &Value) -> MindmapCanonicalConnector {
    let properties = sheet
        .pointer("/rootTopic/style/properties")
        .and_then(Value::as_object)
        .or_else(|| {
            sheet
                .pointer("/theme/centralTopic/properties")
                .and_then(Value::as_object)
        });
    let Some(properties) = properties else {
        return MindmapCanonicalConnector::default();
    };
    MindmapCanonicalConnector {
        kind: properties
            .get("line-class")
            .and_then(Value::as_str)
            .map(connector_kind_name),
        corner_radius: property_string(properties, "line-corner"),
        width: property_string(properties, "line-width"),
        color: property_string(properties, "line-color"),
        pattern: property_string(properties, "line-pattern"),
    }
}

fn connector_kind_name(value: &str) -> String {
    match value {
        "org.xmind.branchConnection.curve" => "curved",
        "org.xmind.branchConnection.straight" => "straight",
        "org.xmind.branchConnection.elbow" => "elbow",
        "org.xmind.branchConnection.roundedElbow" => "rounded-elbow",
        _ => value,
    }
    .to_owned()
}

fn property_string(properties: &Map<String, Value>, key: &str) -> Option<String> {
    match properties.get(key)? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MindmapTree {
    pub topics: BTreeMap<String, MindmapTopic>,
    pub root_id: Option<String>,
    pub structure: String,
    pub structure_class: Option<String>,
    pub connector: MindmapCanonicalConnector,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MindmapCanonicalConnector {
    pub kind: Option<String>,
    pub corner_radius: Option<String>,
    pub width: Option<String>,
    pub color: Option<String>,
    pub pattern: Option<String>,
}

impl MindmapTree {
    pub fn from_content_json(content_json: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(content_json)
            .map_err(|error| format!("invalid_json: content.json 不是有效 JSON：{error}"))?;
        let sheets = root
            .as_array()
            .ok_or_else(|| "invalid_root_shape: content.json 根节点必须是数组".to_string())?;
        let sheet = sheets
            .first()
            .ok_or_else(|| "missing_root_topic: content.json 不能为空".to_string())?;
        let root_topic = sheet
            .get("rootTopic")
            .ok_or_else(|| "missing_root_topic: content.json 不包含 rootTopic".to_string())?;
        let mut tree = Self {
            structure: structure_name(
                sheet
                    .get("rootTopic")
                    .and_then(|root| root.get("structureClass"))
                    .and_then(Value::as_str),
            ),
            structure_class: sheet
                .get("rootTopic")
                .and_then(|root| root.get("structureClass"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            connector: connector_from_sheet(sheet),
            ..Self::default()
        };
        tree.walk_topic(root_topic, None, 0)?;
        if tree.root_id.is_none() {
            return Err("missing_root_topic: rootTopic 无效".to_string());
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
            .ok_or_else(|| "invalid_topic: topic 必须是对象".to_string())?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "missing_topic_id: topic 缺少 id".to_string())?
            .to_owned();
        if self.topics.contains_key(&id) {
            return Err(format!("duplicate_topic_id: topic id 重复：{id}"));
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
        let marker_values = object
            .get("markers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let markers = marker_values
            .iter()
            .filter_map(MindmapMarker::from_json)
            .collect::<Vec<_>>();
        let opaque_marker_count = marker_values.len().saturating_sub(markers.len());
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
        let task = MindmapTaskSemantics::from_topic(object, &markers);
        let task_presentation = task
            .as_ref()
            .and_then(|task| task.task_presentation.clone());
        let todo_status = task.as_ref().and_then(|task| task.todo_status.clone());
        let todo_progress = task.as_ref().and_then(|task| task.todo_progress);
        let is_complete = task
            .as_ref()
            .and_then(|task| task.effective_status.as_deref())
            .is_some_and(|status| status == "done");
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
                opaque_marker_count,
                is_complete,
                task_presentation,
                todo_status,
                todo_progress,
                task,
            },
        );
        for child in child_values {
            self.walk_topic(&child, Some(&id), depth + 1)?;
        }
        Ok(())
    }

    /// Stable projection consumed by the macOS and Windows UI parity tests.
    ///
    /// The projection walks sibling order (not map order), uses document IDs,
    /// excludes paths/timestamps/random values, and embeds XMind 27.1 task and
    /// graph fields so both native parsers can be checked against one golden.
    pub fn canonical_projection(&self) -> Value {
        let mut topics = Vec::new();
        if let Some(root_id) = self.root_id.as_deref() {
            self.collect_canonical_topics(root_id, &mut topics);
        }
        canonical_object(vec![
            ("connector", self.connector.to_canonical_json()),
            ("root_id", optional_string(&self.root_id)),
            ("schema_version", json!(1)),
            ("structure", Value::String(self.structure.clone())),
            ("structure_class", optional_string(&self.structure_class)),
            ("topics", Value::Array(topics)),
        ])
    }

    pub fn canonical_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.canonical_projection()).map_err(|error| error.to_string())
    }

    pub fn canonical_sha256(&self) -> Result<String, String> {
        let canonical = self.canonical_json()?;
        Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
    }

    fn collect_canonical_topics(&self, id: &str, output: &mut Vec<Value>) {
        let Some(topic) = self.topics.get(id) else {
            return;
        };
        output.push(topic.to_canonical_json());
        for child_id in &topic.child_ids {
            self.collect_canonical_topics(child_id, output);
        }
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MindmapEdgeAnchors {
    pub start_x: f32,
    pub start_y: f32,
    pub end_x: f32,
    pub end_y: f32,
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

    /// Choose connectors on the node side that faces the other node. Org charts
    /// and tree charts use vertical anchors; side layouts use the actual branch
    /// direction so left-side mindmap branches and logic-left never cut through
    /// their nodes.
    pub fn edge_anchors(
        &self,
        parent_id: &str,
        child_id: &str,
        structure: MindmapGraphStructureDto,
    ) -> Option<MindmapEdgeAnchors> {
        let parent = self.node(parent_id)?;
        let child = self.node(child_id)?;
        if structure == MindmapGraphStructureDto::OrgChart {
            return Some(MindmapEdgeAnchors {
                start_x: parent.center_x,
                start_y: parent.center_y + parent.height / 2.0,
                end_x: child.center_x,
                end_y: child.center_y - child.height / 2.0,
            });
        }
        if child.center_x < parent.center_x {
            Some(MindmapEdgeAnchors {
                start_x: parent.center_x - parent.width / 2.0,
                start_y: parent.center_y,
                end_x: child.center_x + child.width / 2.0,
                end_y: child.center_y,
            })
        } else {
            Some(MindmapEdgeAnchors {
                start_x: parent.center_x + parent.width / 2.0,
                start_y: parent.center_y,
                end_x: child.center_x - child.width / 2.0,
                end_y: child.center_y,
            })
        }
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
        let marker_chars = topic.marker_summary().chars().count().min(18) as f32;
        let chars = (topic.title.chars().count() as f32 + marker_chars / 2.0).min(34.0);
        ((MIN_NODE_WIDTH + chars * 5.6) * self.zoom)
            .clamp(MIN_NODE_WIDTH * self.zoom, MAX_NODE_WIDTH * self.zoom)
    }

    fn node_height(&self, topic: &MindmapTopic) -> f32 {
        let marker_row = if topic.marker_summary().is_empty() {
            0.0
        } else {
            14.0
        };
        (NODE_HEIGHT + marker_row) * self.zoom
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
                height: self.node_height(topic),
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
                height: self.node_height(root),
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
                height: self.node_height(topic),
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
                height: self.node_height(topic),
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
