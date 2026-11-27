//! Typed, lossless access to XMind topic markers.
//!
//! Modern XMind stores topic markers in `topic.markers` as an ordered array of
//! objects shaped like `{ "markerId": "priority-1" }`. Kite recognizes priority
//! and task-progress markers as typed values and treats every other marker ID as a
//! generic marker. Only `markerId` and the marker slot being edited are rewritten;
//! opaque fields on surviving marker objects and unknown marker entries remain in
//! their original order.

use serde_json::{Map, Value, json};
use thiserror::Error;

pub const TOPIC_MARKERS_FIELD: &str = "markers";
pub const MARKER_ID_FIELD: &str = "markerId";

/// XMind's priority-1 through priority-9 marker IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorityMarker {
    Priority1,
    Priority2,
    Priority3,
    Priority4,
    Priority5,
    Priority6,
    Priority7,
    Priority8,
    Priority9,
}

impl PriorityMarker {
    pub fn from_xmind_id(value: &str) -> Option<Self> {
        Some(match value {
            "priority-1" => Self::Priority1,
            "priority-2" => Self::Priority2,
            "priority-3" => Self::Priority3,
            "priority-4" => Self::Priority4,
            "priority-5" => Self::Priority5,
            "priority-6" => Self::Priority6,
            "priority-7" => Self::Priority7,
            "priority-8" => Self::Priority8,
            "priority-9" => Self::Priority9,
            _ => return None,
        })
    }

    pub fn xmind_id(self) -> &'static str {
        match self {
            Self::Priority1 => "priority-1",
            Self::Priority2 => "priority-2",
            Self::Priority3 => "priority-3",
            Self::Priority4 => "priority-4",
            Self::Priority5 => "priority-5",
            Self::Priority6 => "priority-6",
            Self::Priority7 => "priority-7",
            Self::Priority8 => "priority-8",
            Self::Priority9 => "priority-9",
        }
    }

    pub fn level(self) -> u8 {
        self as u8 + 1
    }
}

/// XMind's task progress marker set.
///
/// XMind represents task progress as one marker rather than a separate typed task
/// object. The percentages are expressed in eighths, which exactly matches every
/// marker XMind currently exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskProgress {
    Start,
    Eighth,
    Quarter,
    ThreeEighths,
    Half,
    FiveEighths,
    ThreeQuarters,
    SevenEighths,
    Done,
    Paused,
}

impl TaskProgress {
    pub fn from_xmind_id(value: &str) -> Option<Self> {
        Some(match value {
            "task-start" => Self::Start,
            "task-oct" => Self::Eighth,
            "task-quarter" => Self::Quarter,
            "task-3oct" => Self::ThreeEighths,
            "task-half" => Self::Half,
            "task-5oct" => Self::FiveEighths,
            "task-3quar" => Self::ThreeQuarters,
            "task-7oct" => Self::SevenEighths,
            "task-done" => Self::Done,
            "task-pause" => Self::Paused,
            _ => return None,
        })
    }

    pub fn xmind_id(self) -> &'static str {
        match self {
            Self::Start => "task-start",
            Self::Eighth => "task-oct",
            Self::Quarter => "task-quarter",
            Self::ThreeEighths => "task-3oct",
            Self::Half => "task-half",
            Self::FiveEighths => "task-5oct",
            Self::ThreeQuarters => "task-3quar",
            Self::SevenEighths => "task-7oct",
            Self::Done => "task-done",
            Self::Paused => "task-pause",
        }
    }

    /// Returns progress in eighths, or `None` for the paused marker.
    pub fn eighths(self) -> Option<u8> {
        match self {
            Self::Start => Some(0),
            Self::Eighth => Some(1),
            Self::Quarter => Some(2),
            Self::ThreeEighths => Some(3),
            Self::Half => Some(4),
            Self::FiveEighths => Some(5),
            Self::ThreeQuarters => Some(6),
            Self::SevenEighths => Some(7),
            Self::Done => Some(8),
            Self::Paused => None,
        }
    }

    pub fn is_complete(self) -> bool {
        self == Self::Done
    }

    pub fn is_paused(self) -> bool {
        self == Self::Paused
    }
}

/// Coarse family used by UI code to choose a marker glyph while retaining the
/// original marker ID for unknown or future members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerFamily {
    Priority,
    Task,
    Tag,
    Flag,
    Star,
    People,
    Smiley,
    Symbol,
    Other,
}

impl MarkerFamily {
    pub fn from_marker_id(marker_id: &str) -> Self {
        if marker_id.starts_with("priority-") {
            Self::Priority
        } else if marker_id.starts_with("task-") {
            Self::Task
        } else if marker_id.starts_with("tag-") {
            Self::Tag
        } else if marker_id.starts_with("flag-") {
            Self::Flag
        } else if marker_id.starts_with("star-") {
            Self::Star
        } else if marker_id.starts_with("people-") {
            Self::People
        } else if marker_id.starts_with("smiley-") {
            Self::Smiley
        } else if marker_id.starts_with("symbol-")
            || marker_id.starts_with("symbol_")
            || marker_id.starts_with("c_symbol_")
        {
            Self::Symbol
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    Priority(PriorityMarker),
    TaskProgress(TaskProgress),
    Generic { family: MarkerFamily },
}

impl MarkerKind {
    pub fn from_marker_id(marker_id: &str) -> Self {
        if let Some(priority) = PriorityMarker::from_xmind_id(marker_id) {
            Self::Priority(priority)
        } else if let Some(progress) = TaskProgress::from_xmind_id(marker_id) {
            Self::TaskProgress(progress)
        } else {
            Self::Generic {
                family: MarkerFamily::from_marker_id(marker_id),
            }
        }
    }
}

/// One recognized marker object. `opaque_fields` contains every property except
/// `markerId`, including future or application-specific marker metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicMarker {
    pub marker_id: String,
    pub kind: MarkerKind,
    pub opaque_fields: Map<String, Value>,
}

/// A non-object marker entry or an object without a usable string `markerId`.
/// It is exposed for diagnostics but never reinterpreted by Kite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueMarkerEntry {
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerEntry {
    Marker(TopicMarker),
    Opaque(OpaqueMarkerEntry),
}

/// Ordered marker set exactly as stored on one topic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicMarkerSet {
    pub entries: Vec<MarkerEntry>,
}

impl TopicMarkerSet {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn priority(&self) -> Option<PriorityMarker> {
        self.entries.iter().find_map(|entry| match entry {
            MarkerEntry::Marker(marker) => match marker.kind {
                MarkerKind::Priority(priority) => Some(priority),
                _ => None,
            },
            MarkerEntry::Opaque(_) => None,
        })
    }

    pub fn task_progress(&self) -> Option<TaskProgress> {
        self.entries.iter().find_map(|entry| match entry {
            MarkerEntry::Marker(marker) => match marker.kind {
                MarkerKind::TaskProgress(progress) => Some(progress),
                _ => None,
            },
            MarkerEntry::Opaque(_) => None,
        })
    }

    pub fn markers(&self) -> impl Iterator<Item = &TopicMarker> {
        self.entries.iter().filter_map(|entry| match entry {
            MarkerEntry::Marker(marker) => Some(marker),
            MarkerEntry::Opaque(_) => None,
        })
    }

    pub fn header(&self) -> TopicHeaderMarkers {
        let mut markers = Vec::new();
        let mut opaque_entries = 0;
        for entry in &self.entries {
            match entry {
                MarkerEntry::Marker(marker) => markers.push(TopicHeaderMarker {
                    marker_id: marker.marker_id.clone(),
                    kind: marker.kind,
                }),
                MarkerEntry::Opaque(_) => opaque_entries += 1,
            }
        }
        TopicHeaderMarkers {
            markers,
            opaque_entries,
        }
    }
}

/// UI-ready marker metadata. Kite does not bake glyphs or pixel spacing into the
/// document model; macOS and Windows can render this shared ordered header model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicHeaderMarkers {
    pub markers: Vec<TopicHeaderMarker>,
    pub opaque_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicHeaderMarker {
    pub marker_id: String,
    pub kind: MarkerKind,
}

/// Three-state slot update used to distinguish "leave unchanged" from "clear".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SlotUpdate<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

/// Marker edit. Priority and task progress are normalized to one marker each;
/// unknown marker objects remain in place. `add` and `remove` address all other
/// marker IDs and preserve their original order where possible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicMarkerPatch {
    pub priority: SlotUpdate<PriorityMarker>,
    pub task_progress: SlotUpdate<TaskProgress>,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopicMarkerError {
    #[error("sheet index {index} is out of range")]
    SheetNotFound { index: usize },
    #[error("sheet {index} has no object-valued rootTopic")]
    MissingRootTopic { index: usize },
    #[error("topic {topic_id:?} was not found in sheet {sheet_index}")]
    TopicNotFound {
        sheet_index: usize,
        topic_id: String,
    },
    #[error("sheet {index} rootTopic.markers must be an array")]
    InvalidMarkersField { index: usize },
    #[error("marker ID must not be empty")]
    EmptyMarkerId,
    #[error(
        "marker ID {0:?} is reserved for a typed priority/task slot; use the corresponding patch field"
    )]
    ReservedMarkerId(String),
}

/// Read the ordered marker set for one topic.
pub fn read_topic_markers(
    content: &Value,
    sheet_index: usize,
    topic_id: &str,
) -> Result<TopicMarkerSet, TopicMarkerError> {
    let topic = topic_at(content, sheet_index, topic_id)?;
    topic_marker_set(topic, sheet_index)
}

/// Apply a marker edit in place. Callers that need package-level atomicity should
/// use `XmindPackage::apply_topic_markers`, which edits a clone first.
pub fn apply_topic_markers(
    content: &mut Value,
    sheet_index: usize,
    topic_id: &str,
    patch: &TopicMarkerPatch,
) -> Result<(), TopicMarkerError> {
    validate_patch(patch)?;

    let topic = topic_at_mut(content, sheet_index, topic_id)?;
    let object = topic
        .as_object_mut()
        .ok_or_else(|| TopicMarkerError::TopicNotFound {
            sheet_index,
            topic_id: topic_id.to_owned(),
        })?;

    if !object.contains_key(TOPIC_MARKERS_FIELD) {
        if !patch_has_mutations(patch) {
            return Ok(());
        }
        object.insert(TOPIC_MARKERS_FIELD.to_owned(), Value::Array(Vec::new()));
    }

    let markers = object
        .get_mut(TOPIC_MARKERS_FIELD)
        .and_then(Value::as_array_mut)
        .ok_or(TopicMarkerError::InvalidMarkersField { index: sheet_index })?;

    for marker_id in &patch.remove {
        markers.retain(|entry| marker_id_of(entry) != Some(marker_id.as_str()));
    }

    apply_priority_slot(markers, &patch.priority);
    apply_task_progress_slot(markers, &patch.task_progress);

    for marker_id in &patch.add {
        if !markers
            .iter()
            .any(|entry| marker_id_of(entry) == Some(marker_id.as_str()))
        {
            markers.push(json!({ MARKER_ID_FIELD: marker_id }));
        }
    }

    Ok(())
}

fn validate_patch(patch: &TopicMarkerPatch) -> Result<(), TopicMarkerError> {
    for marker_id in patch.add.iter().chain(&patch.remove) {
        if marker_id.trim().is_empty() {
            return Err(TopicMarkerError::EmptyMarkerId);
        }
    }
    for marker_id in &patch.add {
        if PriorityMarker::from_xmind_id(marker_id).is_some()
            || TaskProgress::from_xmind_id(marker_id).is_some()
        {
            return Err(TopicMarkerError::ReservedMarkerId(marker_id.clone()));
        }
    }
    Ok(())
}

fn patch_has_mutations(patch: &TopicMarkerPatch) -> bool {
    !matches!(patch.priority, SlotUpdate::Keep)
        || !matches!(patch.task_progress, SlotUpdate::Keep)
        || !patch.add.is_empty()
        || !patch.remove.is_empty()
}

fn apply_priority_slot(markers: &mut Vec<Value>, update: &SlotUpdate<PriorityMarker>) {
    apply_slot(
        markers,
        update,
        |value| value.xmind_id(),
        |marker_id| PriorityMarker::from_xmind_id(marker_id).is_some(),
    );
}

fn apply_task_progress_slot(markers: &mut Vec<Value>, update: &SlotUpdate<TaskProgress>) {
    apply_slot(
        markers,
        update,
        |value| value.xmind_id(),
        |marker_id| TaskProgress::from_xmind_id(marker_id).is_some(),
    );
}

fn apply_slot<T>(
    markers: &mut Vec<Value>,
    update: &SlotUpdate<T>,
    new_marker_id: fn(&T) -> &'static str,
    is_slot_marker: impl Fn(&str) -> bool,
) {
    match update {
        SlotUpdate::Keep => {}
        SlotUpdate::Clear => markers
            .retain(|entry| marker_id_of(entry).is_none_or(|marker_id| !is_slot_marker(marker_id))),
        SlotUpdate::Set(value) => {
            let marker_id = new_marker_id(value);
            let positions: Vec<usize> = markers
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    marker_id_of(entry)
                        .filter(|existing| is_slot_marker(existing))
                        .map(|_| index)
                })
                .collect();

            if let Some(&first) = positions.first()
                && marker_id_of(&markers[first]) == Some(marker_id)
            {
                let mut retained = 0;
                let mut index = 0;
                while index < markers.len() {
                    let matches_slot = marker_id_of(&markers[index]).is_some_and(&is_slot_marker);
                    let duplicate = matches_slot && retained > 0;
                    if duplicate {
                        markers.remove(index);
                    } else {
                        if matches_slot {
                            retained += 1;
                        }
                        index += 1;
                    }
                }
                return;
            }

            let Some(first) = positions.first().copied() else {
                markers.push(json!({ MARKER_ID_FIELD: marker_id }));
                return;
            };
            let mut replacement = markers[first].as_object().cloned().unwrap_or_default();
            replacement.insert(
                MARKER_ID_FIELD.to_owned(),
                Value::String(marker_id.to_owned()),
            );
            markers.retain(|entry| {
                marker_id_of(entry).is_none_or(|existing| !is_slot_marker(existing))
            });
            markers.insert(first.min(markers.len()), Value::Object(replacement));
        }
    }
}

fn topic_marker_set(topic: &Value, sheet_index: usize) -> Result<TopicMarkerSet, TopicMarkerError> {
    let Some(markers) = topic.get(TOPIC_MARKERS_FIELD) else {
        return Ok(TopicMarkerSet::default());
    };
    let markers = markers
        .as_array()
        .ok_or(TopicMarkerError::InvalidMarkersField { index: sheet_index })?;

    let entries = markers
        .iter()
        .map(|entry| {
            let Some(marker_id) = marker_id_of(entry).filter(|value| !value.trim().is_empty())
            else {
                return MarkerEntry::Opaque(OpaqueMarkerEntry {
                    value: entry.clone(),
                });
            };
            let mut opaque_fields = entry.as_object().cloned().unwrap_or_default();
            opaque_fields.remove(MARKER_ID_FIELD);
            MarkerEntry::Marker(TopicMarker {
                marker_id: marker_id.to_owned(),
                kind: MarkerKind::from_marker_id(marker_id),
                opaque_fields,
            })
        })
        .collect();

    Ok(TopicMarkerSet { entries })
}

fn marker_id_of(value: &Value) -> Option<&str> {
    value.get(MARKER_ID_FIELD).and_then(Value::as_str)
}

fn sheet_at(content: &Value, sheet_index: usize) -> Result<&Value, TopicMarkerError> {
    content
        .as_array()
        .and_then(|sheets| sheets.get(sheet_index))
        .ok_or(TopicMarkerError::SheetNotFound { index: sheet_index })
}

fn sheet_at_mut(content: &mut Value, sheet_index: usize) -> Result<&mut Value, TopicMarkerError> {
    content
        .as_array_mut()
        .and_then(|sheets| sheets.get_mut(sheet_index))
        .ok_or(TopicMarkerError::SheetNotFound { index: sheet_index })
}

fn topic_at<'a>(
    content: &'a Value,
    sheet_index: usize,
    topic_id: &str,
) -> Result<&'a Value, TopicMarkerError> {
    let sheet = sheet_at(content, sheet_index)?;
    let root = sheet
        .get("rootTopic")
        .filter(|root| root.is_object())
        .ok_or(TopicMarkerError::MissingRootTopic { index: sheet_index })?;
    find_topic(root, topic_id).ok_or_else(|| TopicMarkerError::TopicNotFound {
        sheet_index,
        topic_id: topic_id.to_owned(),
    })
}

fn topic_at_mut<'a>(
    content: &'a mut Value,
    sheet_index: usize,
    topic_id: &str,
) -> Result<&'a mut Value, TopicMarkerError> {
    let sheet = sheet_at_mut(content, sheet_index)?;
    let root = sheet
        .get_mut("rootTopic")
        .filter(|root| root.is_object())
        .ok_or(TopicMarkerError::MissingRootTopic { index: sheet_index })?;
    find_topic_mut(root, topic_id).ok_or_else(|| TopicMarkerError::TopicNotFound {
        sheet_index,
        topic_id: topic_id.to_owned(),
    })
}

fn find_topic<'a>(topic: &'a Value, topic_id: &str) -> Option<&'a Value> {
    if topic.get("id").and_then(Value::as_str) == Some(topic_id) {
        return Some(topic);
    }
    let children = topic.get("children")?.as_object()?;
    children.values().find_map(|collection| {
        collection
            .as_array()?
            .iter()
            .find_map(|child| find_topic(child, topic_id))
    })
}

fn find_topic_mut<'a>(topic: &'a mut Value, topic_id: &str) -> Option<&'a mut Value> {
    if topic.get("id").and_then(Value::as_str) == Some(topic_id) {
        return Some(topic);
    }
    let children = topic.get_mut("children")?.as_object_mut()?;
    children.values_mut().find_map(|collection| {
        collection
            .as_array_mut()?
            .iter_mut()
            .find_map(|child| find_topic_mut(child, topic_id))
    })
}
