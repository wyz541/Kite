//! Structured TODO semantics layered on XMind's task-progress markers.
//!
//! XMind's modern topic format uses `markers[]` task-* entries as progress state.
//! This module intentionally does not invent a second persisted TODO object. It
//! reads and writes the observed marker mapping, leaves an existing opaque `task`
//! object untouched, and exposes platform-neutral completion rendering semantics.

use serde_json::Value;
use thiserror::Error;

use super::topic_markers::{
    SlotUpdate, TaskProgress, TopicMarkerError, TopicMarkerPatch, TopicMarkerSet,
    apply_topic_markers, read_topic_markers,
};

/// Normalized TODO state derived from the canonical task-progress marker.
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

/// Read a topic's canonical TODO state and completion-rendering semantics.
pub fn read_topic_todo(
    content: &Value,
    sheet_index: usize,
    topic_id: &str,
) -> Result<TopicTodoState, TopicTodoError> {
    let markers = read_topic_markers(content, sheet_index, topic_id)?;
    Ok(todo_state(markers))
}

/// Apply a TODO edit in place. The caller can wrap this in a package transaction
/// when file-level atomicity is required.
pub fn apply_topic_todo(
    content: &mut Value,
    sheet_index: usize,
    topic_id: &str,
    patch: &TopicTodoPatch,
) -> Result<TopicTodoState, TopicTodoError> {
    let current = read_topic_todo(content, sheet_index, topic_id)?;
    let progress = match &patch.update {
        TodoUpdate::Keep => SlotUpdate::Keep,
        TodoUpdate::SetProgress(progress) => SlotUpdate::Set(*progress),
        TodoUpdate::SetCompleted(true) => SlotUpdate::Set(TaskProgress::Done),
        TodoUpdate::SetCompleted(false) => SlotUpdate::Set(TaskProgress::Start),
        TodoUpdate::ToggleCompletion if current.is_complete() => {
            SlotUpdate::Set(TaskProgress::Start)
        }
        TodoUpdate::ToggleCompletion => SlotUpdate::Set(TaskProgress::Done),
    };

    apply_topic_markers(
        content,
        sheet_index,
        topic_id,
        &TopicMarkerPatch {
            task_progress: progress,
            ..TopicMarkerPatch::default()
        },
    )?;
    read_topic_todo(content, sheet_index, topic_id)
}

fn todo_state(markers: TopicMarkerSet) -> TopicTodoState {
    let progress = markers.task_progress();
    let status = TodoStatus::from_progress(progress);
    let rendering = CompletionRendering::for_status(status);
    TopicTodoState {
        status,
        progress,
        rendering,
        markers,
    }
}
