//! Structured TODO semantics for XMind's modern top-level task fields with a
//! legacy marker fallback.
//!
//! XMind 27.x renders task checkboxes and progress from topic-level fields such as
//! `taskPresentation`, `todoStatus`, `todoProgress`, and `taskProgress`. Earlier
//! documents (and Kite's older writer) used one of the `task-*` entries in
//! `markers` instead. This module reads the modern representation first, keeps the
//! legacy marker synchronized on writes, and never replaces the opaque `task`
//! object, notes, or unrelated markers.

use serde_json::{Value, json};
use thiserror::Error;

use super::topic_markers::{
    SlotUpdate, TaskProgress, TopicMarkerError, TopicMarkerPatch, TopicMarkerSet,
    apply_topic_markers, read_topic_markers, topic_at, topic_at_mut,
};

pub const TASK_PRESENTATION_FIELD: &str = "taskPresentation";
pub const TODO_STATUS_FIELD: &str = "todoStatus";
pub const TODO_PROGRESS_FIELD: &str = "todoProgress";
pub const TASK_PROGRESS_FIELD: &str = "taskProgress";
pub const TASK_START_DATE_FIELD: &str = "taskStartDate";
pub const TASK_DUE_DATE_FIELD: &str = "taskDueDate";

/// XMind 27.x task presentation modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskPresentation {
    TodoCheckbox,
    TodoProgress,
    PlannedTask,
    Other(String),
}

impl TaskPresentation {
    pub fn from_xmind_value(value: &str) -> Self {
        match value {
            "todo-checkbox" => Self::TodoCheckbox,
            "todo-progress" => Self::TodoProgress,
            "planned-task" => Self::PlannedTask,
            value => Self::Other(value.to_owned()),
        }
    }

    pub fn xmind_value(&self) -> &str {
        match self {
            Self::TodoCheckbox => "todo-checkbox",
            Self::TodoProgress => "todo-progress",
            Self::PlannedTask => "planned-task",
            Self::Other(value) => value,
        }
    }
}

/// Normalized TODO state derived from modern task fields and, when those are
/// absent, the canonical legacy task-progress marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    NotStarted,
    InProgress { eighths: u8 },
    Paused,
    Done,
}

impl TodoStatus {
    pub fn from_progress(progress: Option<TaskProgress>) -> Self {
        match progress {
            None => Self::NotStarted,
            Some(TaskProgress::Start) => Self::NotStarted,
            Some(TaskProgress::Eighth) => Self::InProgress { eighths: 1 },
            Some(TaskProgress::Quarter) => Self::InProgress { eighths: 2 },
            Some(TaskProgress::ThreeEighths) => Self::InProgress { eighths: 3 },
            Some(TaskProgress::Half) => Self::InProgress { eighths: 4 },
            Some(TaskProgress::FiveEighths) => Self::InProgress { eighths: 5 },
            Some(TaskProgress::ThreeQuarters) => Self::InProgress { eighths: 6 },
            Some(TaskProgress::SevenEighths) => Self::InProgress { eighths: 7 },
            Some(TaskProgress::Done) => Self::Done,
            Some(TaskProgress::Paused) => Self::Paused,
        }
    }

    pub fn is_complete(self) -> bool {
        self == Self::Done
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDecoration {
    None,
    LineThrough,
}

/// Abstract emphasis rather than a platform color token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualTone {
    Normal,
    Muted,
}

/// Abstract opacity rather than a platform-specific numeric value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpacitySemantics {
    Full,
    Reduced,
}

/// Completion semantics consumed by macOS and Windows renderers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionRendering {
    pub text_decoration: TextDecoration,
    pub tone: VisualTone,
    pub text_opacity: OpacitySemantics,
    pub connector_opacity: OpacitySemantics,
}

impl CompletionRendering {
    pub fn for_status(status: TodoStatus) -> Self {
        if status.is_complete() {
            Self {
                text_decoration: TextDecoration::LineThrough,
                tone: VisualTone::Muted,
                text_opacity: OpacitySemantics::Reduced,
                connector_opacity: OpacitySemantics::Reduced,
            }
        } else {
            Self {
                text_decoration: TextDecoration::None,
                tone: VisualTone::Normal,
                text_opacity: OpacitySemantics::Full,
                connector_opacity: OpacitySemantics::Full,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicTodoState {
    pub status: TodoStatus,
    pub progress: Option<TaskProgress>,
    pub presentation: Option<TaskPresentation>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub rendering: CompletionRendering,
    pub markers: TopicMarkerSet,
}

impl TopicTodoState {
    pub fn is_complete(&self) -> bool {
        self.status.is_complete()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TodoUpdate {
    #[default]
    Keep,
    SetProgress(TaskProgress),
    SetCompleted(bool),
    ToggleCompletion,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicTodoPatch {
    pub update: TodoUpdate,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopicTodoError {
    #[error(transparent)]
    Marker(#[from] TopicMarkerError),
}

/// Read a topic's TODO state. Modern task fields take precedence over the legacy
/// `task-*` marker; the marker set is still returned for lossless inspection.
pub fn read_topic_todo(
    content: &Value,
    sheet_index: usize,
    topic_id: &str,
) -> Result<TopicTodoState, TopicTodoError> {
    let markers = read_topic_markers(content, sheet_index, topic_id)?;
    let topic = topic_at(content, sheet_index, topic_id)?;
    Ok(todo_state(topic, markers))
}

/// Apply a TODO edit in place. Both modern task fields and the legacy task marker
/// are kept in sync. The caller can wrap this in a package transaction when
/// file-level atomicity is required.
pub fn apply_topic_todo(
    content: &mut Value,
    sheet_index: usize,
    topic_id: &str,
    patch: &TopicTodoPatch,
) -> Result<TopicTodoState, TopicTodoError> {
    let current = read_topic_todo(content, sheet_index, topic_id)?;
    if patch.update == TodoUpdate::Keep {
        return Ok(current);
    }

    let progress = match &patch.update {
        TodoUpdate::Keep => unreachable!("handled above"),
        TodoUpdate::SetProgress(progress) => *progress,
        TodoUpdate::SetCompleted(true) => TaskProgress::Done,
        TodoUpdate::SetCompleted(false) => TaskProgress::Start,
        TodoUpdate::ToggleCompletion if current.is_complete() => TaskProgress::Start,
        TodoUpdate::ToggleCompletion => TaskProgress::Done,
    };

    let topic = topic_at(content, sheet_index, topic_id)?;
    let prior_ratio = modern_progress_ratio(topic)
        .or_else(|| current.progress.and_then(TaskProgress::ratio))
        .unwrap_or(0.0);
    let presentation = presentation_for_update(task_presentation(topic), progress);

    apply_topic_markers(
        content,
        sheet_index,
        topic_id,
        &TopicMarkerPatch {
            task_progress: SlotUpdate::Set(progress),
            ..TopicMarkerPatch::default()
        },
    )?;

    let ratio = match progress.ratio() {
        Some(ratio) => ratio,
        // A paused task has no numeric legacy marker. Keep its last numeric
        // progress in the modern fields, but never carry a completed `1` into the
        // paused/todo state.
        None if prior_ratio < 1.0 => prior_ratio,
        None => 0.0,
    };
    let object = topic_at_mut(content, sheet_index, topic_id)?
        .as_object_mut()
        .expect("topic lookup guarantees an object");
    object.insert(
        TASK_PRESENTATION_FIELD.to_owned(),
        Value::String(presentation.xmind_value().to_owned()),
    );
    object.insert(
        TODO_STATUS_FIELD.to_owned(),
        Value::String(
            if progress.is_complete() {
                "done"
            } else {
                "todo"
            }
            .to_owned(),
        ),
    );
    object.insert(TODO_PROGRESS_FIELD.to_owned(), json!(ratio));
    object.insert(TASK_PROGRESS_FIELD.to_owned(), json!(ratio));

    read_topic_todo(content, sheet_index, topic_id)
}

fn todo_state(topic: &Value, markers: TopicMarkerSet) -> TopicTodoState {
    let legacy_progress = markers.task_progress();
    let progress = match modern_task_progress(topic) {
        Some(progress)
            if progress != TaskProgress::Done
                && legacy_pause_is_compatible(topic, legacy_progress) =>
        {
            Some(TaskProgress::Paused)
        }
        Some(progress) => Some(progress),
        None => legacy_progress,
    };
    let status = TodoStatus::from_progress(progress);
    let rendering = CompletionRendering::for_status(status);
    TopicTodoState {
        status,
        progress,
        presentation: task_presentation(topic),
        start_date: task_date(topic, TASK_START_DATE_FIELD),
        due_date: task_date(topic, TASK_DUE_DATE_FIELD),
        rendering,
        markers,
    }
}

fn modern_task_progress(topic: &Value) -> Option<TaskProgress> {
    let presentation = task_presentation(topic);
    let status = modern_todo_status(topic);
    let todo_ratio = ratio_field(topic, TODO_PROGRESS_FIELD);
    let task_ratio = ratio_field(topic, TASK_PROGRESS_FIELD);
    let has_modern_state = topic
        .get(TASK_PRESENTATION_FIELD)
        .and_then(Value::as_str)
        .is_some()
        || status.is_some()
        || todo_ratio.is_some()
        || task_ratio.is_some();

    if !has_modern_state {
        return None;
    }
    if status == Some(ModernTodoStatus::Done) {
        return Some(TaskProgress::Done);
    }

    let ratio = match presentation {
        Some(TaskPresentation::TodoCheckbox) => None,
        Some(TaskPresentation::PlannedTask) => task_ratio.or(todo_ratio),
        Some(TaskPresentation::TodoProgress) => todo_ratio.or(task_ratio),
        Some(TaskPresentation::Other(_)) | None => todo_ratio.or(task_ratio),
    };
    Some(
        ratio
            .and_then(TaskProgress::from_ratio)
            .unwrap_or(TaskProgress::Start),
    )
}

fn modern_progress_ratio(topic: &Value) -> Option<f64> {
    if modern_todo_status(topic) == Some(ModernTodoStatus::Done) {
        return Some(1.0);
    }
    match task_presentation(topic) {
        Some(TaskPresentation::TodoCheckbox) => None,
        Some(TaskPresentation::PlannedTask) => ratio_field(topic, TASK_PROGRESS_FIELD)
            .or_else(|| ratio_field(topic, TODO_PROGRESS_FIELD)),
        Some(TaskPresentation::TodoProgress) => ratio_field(topic, TODO_PROGRESS_FIELD)
            .or_else(|| ratio_field(topic, TASK_PROGRESS_FIELD)),
        Some(TaskPresentation::Other(_)) | None => ratio_field(topic, TODO_PROGRESS_FIELD)
            .or_else(|| ratio_field(topic, TASK_PROGRESS_FIELD)),
    }
}

fn legacy_pause_is_compatible(topic: &Value, legacy_progress: Option<TaskProgress>) -> bool {
    legacy_progress == Some(TaskProgress::Paused)
        && modern_todo_status(topic) != Some(ModernTodoStatus::Done)
        && !matches!(
            task_presentation(topic),
            Some(TaskPresentation::TodoCheckbox)
        )
}

fn presentation_for_update(
    existing: Option<TaskPresentation>,
    progress: TaskProgress,
) -> TaskPresentation {
    existing.unwrap_or(match progress {
        TaskProgress::Start | TaskProgress::Done => TaskPresentation::TodoCheckbox,
        _ => TaskPresentation::TodoProgress,
    })
}

fn task_presentation(topic: &Value) -> Option<TaskPresentation> {
    topic
        .get(TASK_PRESENTATION_FIELD)
        .and_then(Value::as_str)
        .map(TaskPresentation::from_xmind_value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModernTodoStatus {
    Todo,
    Done,
}

fn modern_todo_status(topic: &Value) -> Option<ModernTodoStatus> {
    match topic
        .get(TODO_STATUS_FIELD)
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "todo" => Some(ModernTodoStatus::Todo),
        "done" => Some(ModernTodoStatus::Done),
        _ => None,
    }
}

fn task_date(topic: &Value, field: &str) -> Option<String> {
    topic
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn ratio_field(topic: &Value, field: &str) -> Option<f64> {
    let ratio = match topic.get(field)? {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (ratio.is_finite() && (0.0..=1.0).contains(&ratio)).then_some(ratio)
}
