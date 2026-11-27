use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{NaiveDate, NaiveTime};
use serde_json::{Value, json};

use crate::ffi::KiteError;
use crate::mindmap::{
    BorderLinePattern, ConnectorCornerRadius, ConnectorKind, ConnectorStylePatch,
    DateDisplayFormat, DateInsertionPlacement, DateInsertionRequest, DateInsertionTarget,
    DateLocale, DatePartOrder, DatePrecision, DateStyle, DocumentGraphStylePatch, FontFamily,
    FontSize, FontStyle, FontWeight, GraphStructure, MarkerFamily, MarkerKind, NodeCornerRadius,
    NodeStylePatch, PriorityMarker, SlotUpdate, StaticDateValue, StyleColor, StyleMeasurement,
    SystemDatePattern, TaskProgress, TextAlignment, TextStylePatch, TodoStatus, TodoUpdate,
    TopicMarkerPatch, TopicStylePatch, XmindPackage, insert_date_into_topic,
};
use crate::mindmap_import::{
    CodeStrategy, ContentStrategies, ConversionOptions, ConversionPreset, ImageStrategy,
    TableStrategy, detect_source, generate_mindmap, preview_mindmap, supported_import_formats,
};

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapDocumentDto {
    pub path: String,
    pub content_json: String,
    pub source_sha256: String,
    pub sheet_count: u32,
    pub entry_count: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapSaveResultDto {
    pub source_sha256: String,
    pub bytes_written: u64,
    pub byte_identical: bool,
    pub rewritten_entries: Vec<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapImportFormatDto {
    pub id: String,
    pub display_name: String,
    pub extensions: Vec<String>,
    pub supported: bool,
    pub support_note: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapImportSourceDto {
    pub path: String,
    pub file_name: String,
    pub format: MindmapImportFormatDto,
    pub default_output_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapImportPresetDto {
    StructureFirst,
    ExpandedContent,
    StrictOutline,
    SentenceSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapImportTableStrategyDto {
    Note,
    RowsAsTopics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapImportCodeStrategyDto {
    TopicNote,
    ParentNote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapImportImageStrategyDto {
    TopicLink,
    ParentNote,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapConversionOptionsDto {
    pub preset: MindmapImportPresetDto,
    pub split_content: bool,
    pub preserve_original_paragraphs: bool,
    pub merge_short_fragments: bool,
    pub min_fragment_chars: u32,
    pub max_title_chars: u32,
    pub max_nodes: u32,
    pub max_depth: u32,
    pub include_source_path: bool,
    pub table_strategy: MindmapImportTableStrategyDto,
    pub code_strategy: MindmapImportCodeStrategyDto,
    pub image_strategy: MindmapImportImageStrategyDto,
}

impl Default for MindmapConversionOptionsDto {
    fn default() -> Self {
        let options = ConversionOptions::default();
        Self {
            preset: MindmapImportPresetDto::StructureFirst,
            split_content: options.split_content,
            preserve_original_paragraphs: options.preserve_original_paragraphs,
            merge_short_fragments: options.merge_short_fragments,
            min_fragment_chars: options.min_fragment_chars as u32,
            max_title_chars: options.max_title_chars as u32,
            max_nodes: options.max_nodes as u32,
            max_depth: u32::from(options.max_depth),
            include_source_path: options.include_source_path,
            table_strategy: MindmapImportTableStrategyDto::Note,
            code_strategy: MindmapImportCodeStrategyDto::TopicNote,
            image_strategy: MindmapImportImageStrategyDto::TopicLink,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapImportWarningDto {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub location: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapConversionPreviewDto {
    pub source: MindmapImportSourceDto,
    pub output_path: String,
    pub document_title: String,
    pub source_sha256: String,
    pub preset: MindmapImportPresetDto,
    pub node_count: u32,
    pub structure_node_count: u32,
    pub content_node_count: u32,
    pub max_depth: u32,
    pub note_count: u32,
    pub link_count: u32,
    pub attachment_count: u32,
    pub table_count: u32,
    pub warnings: Vec<MindmapImportWarningDto>,
    pub sample_tree_json: String,
    pub sampled_node_count: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapConversionResultDto {
    pub output_path: String,
    pub source_sha256: String,
    pub output_sha256: String,
    pub source_unchanged: bool,
    pub node_count: u32,
    pub max_depth: u32,
    pub warnings: Vec<MindmapImportWarningDto>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapGraphStructureDto {
    MindMap,
    LogicRight,
    LogicLeft,
    OrgChart,
    Tree,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapConnectorKindDto {
    Curved,
    Straight,
    Elbow,
    RoundedElbow,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MindmapDocumentStyleDto {
    pub structure: MindmapGraphStructureDto,
    pub connector_kind: MindmapConnectorKindDto,
    pub connector_corner_radius: Option<u16>,
    pub connector_width: Option<String>,
    pub connector_color: Option<String>,
    pub connector_pattern: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MindmapDocumentStylePatchDto {
    pub structure: MindmapGraphStructureDto,
    pub connector_kind: MindmapConnectorKindDto,
    pub connector_corner_radius: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MindmapTopicMarkerDto {
    pub marker_id: String,
    pub family: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MindmapTopicMarkersDto {
    pub priority_marker_id: Option<String>,
    pub task_progress_marker_id: Option<String>,
    pub markers: Vec<MindmapTopicMarkerDto>,
    pub opaque_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapTodoStatusDto {
    NotStarted,
    InProgress,
    Paused,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MindmapTodoDto {
    pub status: MindmapTodoStatusDto,
    pub progress_eighths: Option<u32>,
    pub is_complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq, uniffi::Record)]
pub struct MindmapTopicStyleDto {
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub font_weight: Option<String>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub text_color: Option<String>,
    pub alignment: Option<String>,
    pub node_fill: Option<String>,
    pub border_color: Option<String>,
    pub border_width: Option<f64>,
    pub border_pattern: Option<String>,
    pub corner_radius: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapDateStyleDto {
    Iso,
    Chinese,
    EnglishUs,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapDatePrecisionDto {
    DateOnly,
    Minutes,
    Seconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapDateTargetDto {
    Title,
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapDatePlacementDto {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MindmapDatePartOrderDto {
    YearMonthDay,
    MonthDayYear,
    DayMonthYear,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MindmapDateRequestDto {
    pub date: String,
    pub time: Option<String>,
    pub style: MindmapDateStyleDto,
    pub precision: MindmapDatePrecisionDto,
    pub target: MindmapDateTargetDto,
    pub placement: MindmapDatePlacementDto,
    pub system_date_order: Option<MindmapDatePartOrderDto>,
    pub system_date_separator: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MindmapDateInsertionResultDto {
    pub document: MindmapDocumentDto,
    pub text: String,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum MindmapEditDto {
    Rename {
        topic_id: String,
        title: String,
    },
    AddChild {
        parent_id: String,
        title: String,
    },
    AddSibling {
        topic_id: String,
        title: String,
    },
    InsertSiblingBefore {
        topic_id: String,
        title: String,
    },
    NewParent {
        topic_id: String,
        title: String,
    },
    MoveUp {
        topic_id: String,
    },
    MoveDown {
        topic_id: String,
    },
    Paste {
        parent_id: String,
        topic_json: String,
    },
    Delete {
        topic_id: String,
    },
}

#[uniffi::export]
pub fn open_mindmap_document(path: String) -> Result<MindmapDocumentDto, KiteError> {
    let package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    document_dto(path, &package)
}

#[uniffi::export]
pub fn edit_mindmap_document(
    path: String,
    expected_source_sha256: String,
    edit: MindmapEditDto,
) -> Result<MindmapDocumentDto, KiteError> {
    let mut package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    if package.summary().source_sha256 != expected_source_sha256 {
        return Err(KiteError::OperationFailed {
            message: "文件已在 Kite 外被修改，请重新打开后再编辑".to_string(),
        });
    }

    package
        .edit_content_json(|content| {
            apply_edit(content, edit).map_err(crate::mindmap::XmindError::Save)
        })
        .map_err(mindmap_error)?;
    package.save().map_err(mindmap_error)?;
    document_dto(path, &package)
}

#[uniffi::export]
pub fn save_mindmap_content(
    path: String,
    expected_source_sha256: String,
    content_json: String,
) -> Result<MindmapSaveResultDto, KiteError> {
    let mut package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    if package.summary().source_sha256 != expected_source_sha256 {
        return Err(KiteError::OperationFailed {
            message: "文件已在 Kite 外被修改，请重新打开后再保存".to_string(),
        });
    }
    let content: Value =
        serde_json::from_str(&content_json).map_err(|error| KiteError::OperationFailed {
            message: format!("Mindmap 内容不是有效 JSON：{error}"),
        })?;
    package.replace_content(content);
    let report = package.save().map_err(mindmap_error)?;
    Ok(MindmapSaveResultDto {
        source_sha256: package.summary().source_sha256,
        bytes_written: report.bytes_written as u64,
        byte_identical: report.byte_identical,
        rewritten_entries: report.rewritten_entries,
    })
}

#[uniffi::export]
pub fn supported_mindmap_import_formats() -> Vec<MindmapImportFormatDto> {
    supported_import_formats()
        .into_iter()
        .map(import_format_dto)
        .collect()
}

#[uniffi::export]
pub fn detect_mindmap_import_source(path: String) -> Option<MindmapImportSourceDto> {
    detect_source(std::path::Path::new(&path)).map(import_source_dto)
}

#[uniffi::export]
pub fn preview_mindmap_conversion(
    source_path: String,
    options: MindmapConversionOptionsDto,
) -> Result<MindmapConversionPreviewDto, KiteError> {
    let (conversion_options, strategies) = import_options(&options)?;
    let preview = preview_mindmap(
        std::path::Path::new(&source_path),
        &conversion_options,
        strategies,
    )
    .map_err(import_error)?;
    Ok(MindmapConversionPreviewDto {
        source: import_source_dto(preview.source),
        output_path: preview.output_path.to_string_lossy().into_owned(),
        document_title: preview.document_title,
        source_sha256: preview.source_sha256,
        preset: import_preset_dto(&preview.report.preset),
        node_count: count_u32(preview.report.node_count),
        structure_node_count: count_u32(preview.report.structure_node_count),
        content_node_count: count_u32(preview.report.content_node_count),
        max_depth: count_u32(preview.report.max_depth),
        note_count: count_u32(preview.report.note_count),
        link_count: count_u32(preview.report.link_count),
        attachment_count: count_u32(preview.report.attachment_count),
        table_count: count_u32(preview.report.table_count),
        warnings: preview
            .report
            .warnings
            .iter()
            .map(import_warning_dto)
            .collect(),
        sample_tree_json: preview.sample_tree_json,
        sampled_node_count: count_u32(preview.sampled_node_count),
    })
}

#[uniffi::export]
pub fn convert_mindmap_file(
    source_path: String,
    expected_source_sha256: String,
    output_path: String,
    overwrite_existing_output: bool,
    options: MindmapConversionOptionsDto,
) -> Result<MindmapConversionResultDto, KiteError> {
    let (conversion_options, strategies) = import_options(&options)?;
    let outcome = generate_mindmap(
        std::path::Path::new(&source_path),
        std::path::Path::new(&output_path),
        &expected_source_sha256,
        overwrite_existing_output,
        &conversion_options,
        strategies,
    )
    .map_err(import_error)?;
    Ok(MindmapConversionResultDto {
        output_path: outcome.output_path.to_string_lossy().into_owned(),
        source_sha256: outcome.source_sha256,
        output_sha256: outcome.output_sha256,
        source_unchanged: outcome.source_unchanged,
        node_count: count_u32(outcome.report.node_count),
        max_depth: count_u32(outcome.report.max_depth),
        warnings: outcome
            .report
            .warnings
            .iter()
            .map(import_warning_dto)
            .collect(),
    })
}

#[uniffi::export]
pub fn read_mindmap_document_style(
    path: String,
    sheet_index: u32,
) -> Result<MindmapDocumentStyleDto, KiteError> {
    let package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    let style = package
        .document_graph_style(sheet_index as usize)
        .map_err(|error| KiteError::OperationFailed {
            message: error.to_string(),
        })?;
    Ok(document_style_dto(style))
}

#[uniffi::export]
pub fn update_mindmap_document_style(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    patch: MindmapDocumentStylePatchDto,
) -> Result<MindmapDocumentDto, KiteError> {
    let structure = graph_structure_from_dto(patch.structure)?;
    let connector_kind = connector_kind_from_dto(patch.connector_kind)?;
    let corner_radius = patch
        .connector_corner_radius
        .map(ConnectorCornerRadius::new)
        .transpose()
        .map_err(|error| KiteError::OperationFailed {
            message: error.to_string(),
        })?;
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .apply_document_graph_style(
                sheet_index as usize,
                &DocumentGraphStylePatch {
                    structure: Some(structure),
                    connector: Some(ConnectorStylePatch {
                        kind: Some(connector_kind),
                        corner_radius,
                        ..ConnectorStylePatch::default()
                    }),
                },
            )
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn read_mindmap_topic_markers(
    path: String,
    sheet_index: u32,
    topic_id: String,
) -> Result<MindmapTopicMarkersDto, KiteError> {
    let package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    let markers = package
        .topic_markers(sheet_index as usize, &topic_id)
        .map_err(|error| KiteError::OperationFailed {
            message: error.to_string(),
        })?;
    Ok(topic_markers_dto(&markers))
}

#[uniffi::export]
pub fn set_mindmap_priority(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    level: u8,
) -> Result<MindmapDocumentDto, KiteError> {
    let priority = if level == 0 {
        SlotUpdate::Clear
    } else {
        let marker_id = format!("priority-{level}");
        let value = PriorityMarker::from_xmind_id(&marker_id).ok_or_else(|| {
            KiteError::OperationFailed {
                message: format!("优先级必须是 0-9，当前为 {level}"),
            }
        })?;
        SlotUpdate::Set(value)
    };
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .apply_topic_markers(
                sheet_index as usize,
                &topic_id,
                &TopicMarkerPatch {
                    priority,
                    ..TopicMarkerPatch::default()
                },
            )
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn set_mindmap_task_progress(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    marker_id: String,
) -> Result<MindmapDocumentDto, KiteError> {
    let progress = if marker_id.trim().is_empty() {
        SlotUpdate::Clear
    } else {
        let value =
            TaskProgress::from_xmind_id(&marker_id).ok_or_else(|| KiteError::OperationFailed {
                message: format!("未知任务进度标记：{marker_id}"),
            })?;
        SlotUpdate::Set(value)
    };
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .apply_topic_markers(
                sheet_index as usize,
                &topic_id,
                &TopicMarkerPatch {
                    task_progress: progress,
                    ..TopicMarkerPatch::default()
                },
            )
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn add_mindmap_marker(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    marker_id: String,
) -> Result<MindmapDocumentDto, KiteError> {
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .apply_topic_markers(
                sheet_index as usize,
                &topic_id,
                &TopicMarkerPatch {
                    add: vec![marker_id],
                    ..TopicMarkerPatch::default()
                },
            )
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn remove_mindmap_marker(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    marker_id: String,
) -> Result<MindmapDocumentDto, KiteError> {
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .remove_topic_marker(sheet_index as usize, &topic_id, &marker_id)
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn read_mindmap_topic_todo(
    path: String,
    sheet_index: u32,
    topic_id: String,
) -> Result<MindmapTodoDto, KiteError> {
    let package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    let state = package
        .topic_todo(sheet_index as usize, &topic_id)
        .map_err(|error| KiteError::OperationFailed {
            message: error.to_string(),
        })?;
    Ok(todo_dto(state.status))
}

#[uniffi::export]
pub fn toggle_mindmap_todo(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
) -> Result<MindmapDocumentDto, KiteError> {
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .apply_topic_todo(
                sheet_index as usize,
                &topic_id,
                &crate::mindmap::TopicTodoPatch {
                    update: TodoUpdate::ToggleCompletion,
                },
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[uniffi::export]
pub fn insert_mindmap_date(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    request: MindmapDateRequestDto,
) -> Result<MindmapDateInsertionResultDto, KiteError> {
    let date = NaiveDate::parse_from_str(&request.date, "%Y-%m-%d").map_err(|error| {
        KiteError::OperationFailed {
            message: format!("日期格式必须是 YYYY-MM-DD：{error}"),
        }
    })?;
    let time = request
        .time
        .as_deref()
        .map(|value| NaiveTime::parse_from_str(value, "%H:%M:%S"))
        .transpose()
        .map_err(|error| KiteError::OperationFailed {
            message: format!("时间格式必须是 HH:MM:SS：{error}"),
        })?;
    let mut value = StaticDateValue::new(date);
    if let Some(time) = time {
        value = value.with_time(time);
    }
    let format = date_format_from_dto(&request)?;
    let insertion = DateInsertionRequest {
        value,
        format,
        target: date_target_from_dto(request.target),
        placement: date_placement_from_dto(request.placement),
    };

    let mut rendered = String::new();
    let document = mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .edit_content_json(|content| {
                let sheet = content
                    .as_array_mut()
                    .and_then(|sheets| sheets.get_mut(sheet_index as usize))
                    .ok_or_else(|| crate::mindmap::XmindError::Save("sheet 不存在".to_string()))?;
                let root = sheet.get_mut("rootTopic").ok_or_else(|| {
                    crate::mindmap::XmindError::Save("缺少 rootTopic".to_string())
                })?;
                let topic = find_topic_mut(root, &topic_id).ok_or_else(|| {
                    crate::mindmap::XmindError::Save(format!("找不到主题 {topic_id}"))
                })?;
                let result = insert_date_into_topic(topic, &insertion)
                    .map_err(|error| crate::mindmap::XmindError::Save(error.to_string()))?;
                rendered = result.text;
                Ok(())
            })
            .map_err(|error| error.to_string())
    })?;

    Ok(MindmapDateInsertionResultDto {
        document,
        text: rendered,
    })
}

#[uniffi::export]
pub fn read_mindmap_topic_style(
    path: String,
    sheet_index: u32,
    topic_id: String,
) -> Result<MindmapTopicStyleDto, KiteError> {
    let package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    let topic = find_topic_in_sheet(package.content(), sheet_index as usize, &topic_id)
        .ok_or_else(|| KiteError::OperationFailed {
            message: format!("找不到主题 {topic_id}"),
        })?;
    let style =
        crate::mindmap::read_topic_style(topic).map_err(|error| KiteError::OperationFailed {
            message: error.to_string(),
        })?;
    Ok(topic_style_dto(style))
}

#[uniffi::export]
pub fn update_mindmap_topic_style(
    path: String,
    expected_source_sha256: String,
    sheet_index: u32,
    topic_id: String,
    style: MindmapTopicStyleDto,
) -> Result<MindmapDocumentDto, KiteError> {
    let patch = topic_style_patch_from_dto(style)?;
    mutate_mindmap_document(path, expected_source_sha256, |package| {
        package
            .edit_content_json(|content| {
                let sheet = content
                    .as_array_mut()
                    .and_then(|sheets| sheets.get_mut(sheet_index as usize))
                    .ok_or_else(|| crate::mindmap::XmindError::Save("sheet 不存在".to_string()))?;
                let root = sheet.get_mut("rootTopic").ok_or_else(|| {
                    crate::mindmap::XmindError::Save("缺少 rootTopic".to_string())
                })?;
                let topic = find_topic_mut(root, &topic_id).ok_or_else(|| {
                    crate::mindmap::XmindError::Save(format!("找不到主题 {topic_id}"))
                })?;
                crate::mindmap::apply_topic_style(topic, &patch)
                    .map_err(|error| crate::mindmap::XmindError::Save(error.to_string()))
            })
            .map_err(|error| error.to_string())
    })
}

fn mutate_mindmap_document(
    path: String,
    expected_source_sha256: String,
    mutate: impl FnOnce(&mut XmindPackage) -> Result<(), String>,
) -> Result<MindmapDocumentDto, KiteError> {
    let mut package = XmindPackage::open(path.as_ref()).map_err(mindmap_error)?;
    if package.summary().source_sha256 != expected_source_sha256 {
        return Err(KiteError::OperationFailed {
            message: "文件已在 Kite 外被修改，请重新打开后再编辑".to_string(),
        });
    }
    mutate(&mut package).map_err(|message| KiteError::OperationFailed { message })?;
    package.save().map_err(mindmap_error)?;
    document_dto(path, &package)
}

fn document_style_dto(style: crate::mindmap::DocumentGraphStyle) -> MindmapDocumentStyleDto {
    let structure = style
        .structure
        .as_ref()
        .map(graph_structure_dto)
        .unwrap_or(MindmapGraphStructureDto::Unknown);
    let connector = style.connector;
    MindmapDocumentStyleDto {
        structure,
        connector_kind: connector
            .as_ref()
            .map(|connector| connector_kind_dto(&connector.kind))
            .unwrap_or(MindmapConnectorKindDto::Unknown),
        connector_corner_radius: connector
            .as_ref()
            .and_then(|connector| connector.corner_radius)
            .map(ConnectorCornerRadius::value),
        connector_width: connector
            .as_ref()
            .and_then(|connector| connector.width.as_ref())
            .map(|width| width.as_str().to_owned()),
        connector_color: connector
            .as_ref()
            .and_then(|connector| connector.color.as_ref())
            .map(|color| color.as_str().to_owned()),
        connector_pattern: connector
            .as_ref()
            .and_then(|connector| connector.pattern.as_ref())
            .map(|pattern| pattern.to_string()),
    }
}

fn graph_structure_dto(structure: &GraphStructure) -> MindmapGraphStructureDto {
    match structure {
        GraphStructure::MindMap => MindmapGraphStructureDto::MindMap,
        GraphStructure::LogicRight => MindmapGraphStructureDto::LogicRight,
        GraphStructure::LogicLeft => MindmapGraphStructureDto::LogicLeft,
        GraphStructure::OrgChart => MindmapGraphStructureDto::OrgChart,
        GraphStructure::Tree => MindmapGraphStructureDto::Tree,
        GraphStructure::Other(_) => MindmapGraphStructureDto::Unknown,
    }
}

fn graph_structure_from_dto(value: MindmapGraphStructureDto) -> Result<GraphStructure, KiteError> {
    match value {
        MindmapGraphStructureDto::MindMap => Ok(GraphStructure::MindMap),
        MindmapGraphStructureDto::LogicRight => Ok(GraphStructure::LogicRight),
        MindmapGraphStructureDto::LogicLeft => Ok(GraphStructure::LogicLeft),
        MindmapGraphStructureDto::OrgChart => Ok(GraphStructure::OrgChart),
        MindmapGraphStructureDto::Tree => Ok(GraphStructure::Tree),
        MindmapGraphStructureDto::Unknown => Err(KiteError::OperationFailed {
            message: "当前图结构不受支持，不能改写原值".to_string(),
        }),
    }
}

fn connector_kind_dto(kind: &ConnectorKind) -> MindmapConnectorKindDto {
    match kind {
        ConnectorKind::Curved => MindmapConnectorKindDto::Curved,
        ConnectorKind::Straight => MindmapConnectorKindDto::Straight,
        ConnectorKind::Elbow => MindmapConnectorKindDto::Elbow,
        ConnectorKind::RoundedElbow => MindmapConnectorKindDto::RoundedElbow,
        ConnectorKind::Other(_) => MindmapConnectorKindDto::Unknown,
    }
}

fn connector_kind_from_dto(value: MindmapConnectorKindDto) -> Result<ConnectorKind, KiteError> {
    match value {
        MindmapConnectorKindDto::Curved => Ok(ConnectorKind::Curved),
        MindmapConnectorKindDto::Straight => Ok(ConnectorKind::Straight),
        MindmapConnectorKindDto::Elbow => Ok(ConnectorKind::Elbow),
        MindmapConnectorKindDto::RoundedElbow => Ok(ConnectorKind::RoundedElbow),
        MindmapConnectorKindDto::Unknown => Err(KiteError::OperationFailed {
            message: "当前连线样式不受支持，不能改写原值".to_string(),
        }),
    }
}

fn topic_markers_dto(markers: &crate::mindmap::TopicMarkerSet) -> MindmapTopicMarkersDto {
    let typed = markers.markers().collect::<Vec<_>>();
    MindmapTopicMarkersDto {
        priority_marker_id: markers.priority().map(|value| value.xmind_id().to_owned()),
        task_progress_marker_id: markers
            .task_progress()
            .map(|value| value.xmind_id().to_owned()),
        markers: typed
            .iter()
            .map(|marker| MindmapTopicMarkerDto {
                marker_id: marker.marker_id.clone(),
                family: marker_family_name(marker.kind).to_owned(),
                label: marker_label(&marker.marker_id),
            })
            .collect(),
        opaque_count: u32::try_from(markers.entries.len().saturating_sub(typed.len()))
            .unwrap_or(u32::MAX),
    }
}

fn marker_family_name(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Priority(_) => "priority",
        MarkerKind::TaskProgress(_) => "task",
        MarkerKind::Generic { family } => match family {
            MarkerFamily::Priority => "priority",
            MarkerFamily::Task => "task",
            MarkerFamily::Tag => "tag",
            MarkerFamily::Flag => "flag",
            MarkerFamily::Star => "star",
            MarkerFamily::People => "people",
            MarkerFamily::Smiley => "smiley",
            MarkerFamily::Symbol => "symbol",
            MarkerFamily::Other => "other",
        },
    }
}

fn marker_label(marker_id: &str) -> String {
    marker_id
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn todo_dto(status: TodoStatus) -> MindmapTodoDto {
    match status {
        TodoStatus::NotStarted => MindmapTodoDto {
            status: MindmapTodoStatusDto::NotStarted,
            progress_eighths: Some(0),
            is_complete: false,
        },
        TodoStatus::InProgress { eighths } => MindmapTodoDto {
            status: MindmapTodoStatusDto::InProgress,
            progress_eighths: Some(u32::from(eighths)),
            is_complete: false,
        },
        TodoStatus::Paused => MindmapTodoDto {
            status: MindmapTodoStatusDto::Paused,
            progress_eighths: None,
            is_complete: false,
        },
        TodoStatus::Done => MindmapTodoDto {
            status: MindmapTodoStatusDto::Done,
            progress_eighths: Some(8),
            is_complete: true,
        },
    }
}

fn date_format_from_dto(request: &MindmapDateRequestDto) -> Result<DateDisplayFormat, KiteError> {
    let precision = match request.precision {
        MindmapDatePrecisionDto::DateOnly => DatePrecision::DateOnly,
        MindmapDatePrecisionDto::Minutes => DatePrecision::Minutes,
        MindmapDatePrecisionDto::Seconds => DatePrecision::Seconds,
    };
    let style = match request.style {
        MindmapDateStyleDto::Iso => DateStyle::Iso,
        MindmapDateStyleDto::Chinese => DateStyle::Locale(DateLocale::Chinese),
        MindmapDateStyleDto::EnglishUs => DateStyle::Locale(DateLocale::EnglishUnitedStates),
        MindmapDateStyleDto::System => {
            let order = request
                .system_date_order
                .ok_or_else(|| KiteError::OperationFailed {
                    message: "系统日期格式缺少日期顺序".to_string(),
                })?;
            let order = match order {
                MindmapDatePartOrderDto::YearMonthDay => DatePartOrder::YearMonthDay,
                MindmapDatePartOrderDto::MonthDayYear => DatePartOrder::MonthDayYear,
                MindmapDatePartOrderDto::DayMonthYear => DatePartOrder::DayMonthYear,
            };
            let mut separators = request
                .system_date_separator
                .as_deref()
                .unwrap_or("/")
                .chars();
            let separator = separators.next().unwrap_or('/');
            if separators.next().is_some() {
                return Err(KiteError::OperationFailed {
                    message: "系统日期分隔符只能是一个字符".to_string(),
                });
            }
            DateStyle::System(SystemDatePattern { order, separator })
        }
    };
    Ok(DateDisplayFormat { style, precision })
}

fn date_target_from_dto(value: MindmapDateTargetDto) -> DateInsertionTarget {
    match value {
        MindmapDateTargetDto::Title => DateInsertionTarget::Title,
        MindmapDateTargetDto::Note => DateInsertionTarget::Note,
    }
}

fn date_placement_from_dto(value: MindmapDatePlacementDto) -> DateInsertionPlacement {
    match value {
        MindmapDatePlacementDto::Start => DateInsertionPlacement::Start,
        MindmapDatePlacementDto::End => DateInsertionPlacement::End,
    }
}

fn topic_style_dto(style: crate::mindmap::TopicStyle) -> MindmapTopicStyleDto {
    let font_weight = style.text.font_weight.as_ref().map(|weight| match weight {
        FontWeight::Inherited => "inherited".to_string(),
        FontWeight::Normal => "normal".to_string(),
        FontWeight::Bold => "bold".to_string(),
        FontWeight::Numeric(value) => value.to_string(),
        FontWeight::Other(value) => value.clone(),
    });
    let alignment = style
        .text
        .alignment
        .as_ref()
        .map(|alignment| match alignment {
            TextAlignment::Inherited => "inherited".to_string(),
            TextAlignment::Left => "left".to_string(),
            TextAlignment::Center => "center".to_string(),
            TextAlignment::Right => "right".to_string(),
            TextAlignment::Justify => "justify".to_string(),
            TextAlignment::Other(value) => value.clone(),
        });
    let border_pattern = style
        .node
        .border_pattern
        .as_ref()
        .map(|pattern| match pattern {
            BorderLinePattern::Inherited => "inherited".to_string(),
            BorderLinePattern::None => "none".to_string(),
            BorderLinePattern::Solid => "solid".to_string(),
            BorderLinePattern::Dash => "dash".to_string(),
            BorderLinePattern::Dot => "dot".to_string(),
            BorderLinePattern::DashDot => "dash-dot".to_string(),
            BorderLinePattern::Other(value) => value.clone(),
        });
    MindmapTopicStyleDto {
        font_family: style
            .text
            .font_family
            .as_ref()
            .map(|family| family.as_str().to_owned()),
        font_size: style.text.font_size.map(FontSize::points_value),
        font_weight,
        italic: style
            .text
            .font_style
            .as_ref()
            .map(|value| matches!(value, FontStyle::Italic)),
        underline: style.text.decoration.map(|value| value.underline),
        strike: style.text.decoration.map(|value| value.strike),
        text_color: style
            .text
            .color
            .as_ref()
            .map(|color| color.as_str().to_owned()),
        alignment,
        node_fill: style
            .node
            .background
            .as_ref()
            .map(|color| color.as_str().to_owned()),
        border_color: style
            .node
            .border_color
            .as_ref()
            .map(|color| color.as_str().to_owned()),
        border_width: style.node.border_width.map(|measurement| measurement.value),
        border_pattern,
        corner_radius: style.node.corner_radius.map(NodeCornerRadius::value),
    }
}

fn topic_style_patch_from_dto(style: MindmapTopicStyleDto) -> Result<TopicStylePatch, KiteError> {
    let font_family = style
        .font_family
        .filter(|value| !value.trim().is_empty())
        .map(FontFamily::new)
        .transpose()
        .map_err(style_error)?;
    let font_size = style
        .font_size
        .filter(|value| *value > 0.0)
        .map(FontSize::points)
        .transpose()
        .map_err(style_error)?;
    let font_weight = style
        .font_weight
        .map(|value| match value.as_str() {
            "inherited" => Ok(FontWeight::Inherited),
            "normal" | "400" => Ok(FontWeight::Normal),
            "bold" | "700" => Ok(FontWeight::Bold),
            value => value.parse::<u16>().map_or_else(
                |_| Ok(FontWeight::Other(value.to_owned())),
                |value| FontWeight::numeric(value).map_err(style_error),
            ),
        })
        .transpose()?;
    let font_style = style.italic.map(|italic| {
        if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }
    });
    let color = style
        .text_color
        .map(StyleColor::new)
        .transpose()
        .map_err(style_error)?;
    let alignment = style.alignment.map(|value| match value.as_str() {
        "inherited" => TextAlignment::Inherited,
        "center" => TextAlignment::Center,
        "right" => TextAlignment::Right,
        "justify" => TextAlignment::Justify,
        "left" => TextAlignment::Left,
        value => TextAlignment::Other(value.to_owned()),
    });
    let background = style
        .node_fill
        .map(StyleColor::new)
        .transpose()
        .map_err(style_error)?;
    let border_color = style
        .border_color
        .map(StyleColor::new)
        .transpose()
        .map_err(style_error)?;
    let border_width = style
        .border_width
        .filter(|value| *value >= 0.0)
        .map(StyleMeasurement::points)
        .transpose()
        .map_err(style_error)?;
    let border_pattern = style.border_pattern.map(|value| match value.as_str() {
        "inherited" => BorderLinePattern::Inherited,
        "none" => BorderLinePattern::None,
        "dash" => BorderLinePattern::Dash,
        "dot" => BorderLinePattern::Dot,
        "dash-dot" => BorderLinePattern::DashDot,
        "solid" => BorderLinePattern::Solid,
        value => BorderLinePattern::Other(value.to_owned()),
    });
    let corner_radius = style
        .corner_radius
        .map(NodeCornerRadius::new)
        .transpose()
        .map_err(style_error)?;

    Ok(TopicStylePatch {
        text: TextStylePatch {
            font_family,
            font_size,
            font_weight,
            font_style,
            underline: style.underline,
            strike: style.strike,
            color,
            alignment,
            ..TextStylePatch::default()
        },
        node: NodeStylePatch {
            background,
            border_color,
            border_width,
            border_pattern,
            corner_radius,
        },
    })
}

fn style_error(error: crate::mindmap::TopicStyleError) -> KiteError {
    KiteError::OperationFailed {
        message: error.to_string(),
    }
}

fn import_format_dto(format: crate::mindmap_import::ImportFormatInfo) -> MindmapImportFormatDto {
    MindmapImportFormatDto {
        id: format.id,
        display_name: format.display_name,
        extensions: format.extensions,
        supported: format.supported,
        support_note: format.support_note,
    }
}

fn import_source_dto(source: crate::mindmap_import::ImportSourceInfo) -> MindmapImportSourceDto {
    MindmapImportSourceDto {
        path: source.path.to_string_lossy().into_owned(),
        file_name: source.file_name,
        format: import_format_dto(source.format),
        default_output_path: source.default_output_path.to_string_lossy().into_owned(),
    }
}

fn import_options(
    options: &MindmapConversionOptionsDto,
) -> Result<(ConversionOptions, ContentStrategies), KiteError> {
    let conversion = ConversionOptions {
        preset: import_preset(&options.preset),
        split_content: options.split_content,
        preserve_original_paragraphs: options.preserve_original_paragraphs,
        merge_short_fragments: options.merge_short_fragments,
        min_fragment_chars: usize::try_from(options.min_fragment_chars).unwrap_or(usize::MAX),
        max_title_chars: usize::try_from(options.max_title_chars).unwrap_or(usize::MAX),
        max_nodes: usize::try_from(options.max_nodes).unwrap_or(usize::MAX),
        max_depth: u8::try_from(options.max_depth).unwrap_or(u8::MAX),
        include_source_path: options.include_source_path,
    };
    conversion.validate().map_err(import_error)?;
    let strategies = ContentStrategies {
        table: match options.table_strategy {
            MindmapImportTableStrategyDto::Note => TableStrategy::Note,
            MindmapImportTableStrategyDto::RowsAsTopics => TableStrategy::RowsAsTopics,
        },
        code: match options.code_strategy {
            MindmapImportCodeStrategyDto::TopicNote => CodeStrategy::TopicNote,
            MindmapImportCodeStrategyDto::ParentNote => CodeStrategy::ParentNote,
        },
        image: match options.image_strategy {
            MindmapImportImageStrategyDto::TopicLink => ImageStrategy::TopicLink,
            MindmapImportImageStrategyDto::ParentNote => ImageStrategy::ParentNote,
        },
    };
    Ok((conversion, strategies))
}

fn import_preset(preset: &MindmapImportPresetDto) -> ConversionPreset {
    match preset {
        MindmapImportPresetDto::StructureFirst => ConversionPreset::StructureFirst,
        MindmapImportPresetDto::ExpandedContent => ConversionPreset::ExpandedContent,
        MindmapImportPresetDto::StrictOutline => ConversionPreset::StrictOutline,
        MindmapImportPresetDto::SentenceSplit => ConversionPreset::SentenceSplit,
    }
}

fn import_preset_dto(preset: &ConversionPreset) -> MindmapImportPresetDto {
    match preset {
        ConversionPreset::StructureFirst => MindmapImportPresetDto::StructureFirst,
        ConversionPreset::ExpandedContent => MindmapImportPresetDto::ExpandedContent,
        ConversionPreset::StrictOutline => MindmapImportPresetDto::StrictOutline,
        ConversionPreset::SentenceSplit => MindmapImportPresetDto::SentenceSplit,
    }
}

fn import_warning_dto(
    warning: &crate::mindmap_import::ConversionWarning,
) -> MindmapImportWarningDto {
    MindmapImportWarningDto {
        code: format!("{:?}", warning.code),
        severity: match warning.severity {
            crate::mindmap_import::WarningSeverity::Info => "info".to_string(),
            crate::mindmap_import::WarningSeverity::Warning => "warning".to_string(),
            crate::mindmap_import::WarningSeverity::Error => "error".to_string(),
        },
        message: warning.message.clone(),
        location: warning.source.as_ref().map(|source| {
            let mut parts = Vec::new();
            if !source.path.is_empty() {
                parts.push(source.path.join(" / "));
            }
            if let Some(line) = source.line {
                parts.push(format!("第 {line} 行"));
            }
            if let Some(column) = source.column {
                parts.push(format!("第 {column} 列"));
            }
            parts.join(" · ")
        }),
    }
}

fn import_error(error: crate::mindmap_import::ImportError) -> KiteError {
    KiteError::OperationFailed {
        message: error.to_string(),
    }
}

fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn find_topic_in_sheet<'a>(
    content: &'a Value,
    sheet_index: usize,
    topic_id: &str,
) -> Option<&'a Value> {
    let root = content.as_array()?.get(sheet_index)?.get("rootTopic")?;
    find_topic(root, topic_id)
}

fn find_topic<'a>(topic: &'a Value, id: &str) -> Option<&'a Value> {
    if topic.get("id").and_then(Value::as_str) == Some(id) {
        return Some(topic);
    }
    if let Some(children) = topic
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)
    {
        for child in children {
            if let Some(found) = find_topic(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn document_dto(path: String, package: &XmindPackage) -> Result<MindmapDocumentDto, KiteError> {
    let summary = package.summary();
    let content_json =
        serde_json::to_string(package.content()).map_err(|error| KiteError::OperationFailed {
            message: format!("无法序列化 Mindmap 内容：{error}"),
        })?;
    Ok(MindmapDocumentDto {
        path,
        content_json,
        source_sha256: summary.source_sha256,
        sheet_count: u32::try_from(summary.sheet_count).unwrap_or(u32::MAX),
        entry_count: u32::try_from(summary.entry_count).unwrap_or(u32::MAX),
    })
}

fn mindmap_error(error: crate::mindmap::XmindError) -> KiteError {
    KiteError::OperationFailed {
        message: error.to_string(),
    }
}

fn apply_edit(content: &mut Value, edit: MindmapEditDto) -> Result<(), String> {
    let sheets = content
        .as_array_mut()
        .ok_or_else(|| "content.json 根节点必须是数组".to_string())?;
    let sheet = sheets
        .first_mut()
        .ok_or_else(|| "content.json 不能为空".to_string())?;
    let root = sheet
        .get_mut("rootTopic")
        .ok_or_else(|| "文档缺少 rootTopic".to_string())?;

    match edit {
        MindmapEditDto::Rename { topic_id, title } => {
            let topic =
                find_topic_mut(root, &topic_id).ok_or_else(|| format!("找不到主题 {topic_id}"))?;
            topic["title"] = Value::String(normalize_title(title));
        }
        MindmapEditDto::AddChild { parent_id, title } => {
            let parent = find_topic_mut(root, &parent_id)
                .ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
            ensure_attached_children(parent).push(new_topic(title));
        }
        MindmapEditDto::AddSibling { topic_id, title } => {
            insert_sibling(root, &topic_id, new_topic(title), false)?;
        }
        MindmapEditDto::InsertSiblingBefore { topic_id, title } => {
            insert_sibling(root, &topic_id, new_topic(title), true)?;
        }
        MindmapEditDto::NewParent { topic_id, title } => {
            insert_parent(root, &topic_id, title)?;
        }
        MindmapEditDto::MoveUp { topic_id } => {
            move_sibling(root, &topic_id, -1)?;
        }
        MindmapEditDto::MoveDown { topic_id } => {
            move_sibling(root, &topic_id, 1)?;
        }
        MindmapEditDto::Paste {
            parent_id,
            topic_json,
        } => {
            let pasted: Value = serde_json::from_str(&topic_json)
                .map_err(|error| format!("剪贴板主题不是有效 JSON：{error}"))?;
            if !pasted.is_object() {
                return Err("剪贴板主题必须是 JSON 对象".to_string());
            }
            let parent = find_topic_mut(root, &parent_id)
                .ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
            ensure_attached_children(parent).push(assign_new_ids(pasted));
        }
        MindmapEditDto::Delete { topic_id } => {
            let parent_id =
                find_parent_id(root, &topic_id).ok_or_else(|| "根主题不能删除".to_string())?;
            let parent = find_topic_mut(root, &parent_id)
                .ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
            let children = ensure_attached_children(parent);
            let index = children
                .iter()
                .position(|child| {
                    child.get("id").and_then(Value::as_str) == Some(topic_id.as_str())
                })
                .ok_or_else(|| format!("找不到主题 {topic_id}"))?;
            children.remove(index);
        }
    }
    Ok(())
}

fn find_topic_mut<'a>(topic: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if topic.get("id").and_then(Value::as_str) == Some(id) {
        return Some(topic);
    }
    for child in attached_children_mut(topic).into_iter().flatten() {
        if let Some(found) = find_topic_mut(child, id) {
            return Some(found);
        }
    }
    None
}

fn find_parent_id(topic: &Value, id: &str) -> Option<String> {
    let parent_id = topic.get("id").and_then(Value::as_str)?;
    if let Some(children) = topic
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)
    {
        for child in children {
            if child.get("id").and_then(Value::as_str) == Some(id) {
                return Some(parent_id.to_string());
            }
            if let Some(found) = find_parent_id(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn attached_children_mut(topic: &mut Value) -> Option<&mut Vec<Value>> {
    topic
        .get_mut("children")
        .and_then(|children| children.get_mut("attached"))
        .and_then(Value::as_array_mut)
}

fn insert_sibling(
    root: &mut Value,
    topic_id: &str,
    topic: Value,
    before: bool,
) -> Result<(), String> {
    if root.get("id").and_then(Value::as_str) == Some(topic_id) {
        ensure_attached_children(root).push(topic);
        return Ok(());
    }
    let parent_id =
        find_parent_id(root, topic_id).ok_or_else(|| format!("找不到主题的父级 {topic_id}"))?;
    let parent =
        find_topic_mut(root, &parent_id).ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
    let children = ensure_attached_children(parent);
    let index = children
        .iter()
        .position(|child| child.get("id").and_then(Value::as_str) == Some(topic_id))
        .ok_or_else(|| format!("找不到主题 {topic_id}"))?;
    children.insert(if before { index } else { index + 1 }, topic);
    Ok(())
}

fn insert_parent(root: &mut Value, topic_id: &str, title: String) -> Result<(), String> {
    let parent_id =
        find_parent_id(root, topic_id).ok_or_else(|| "根主题不能直接添加父主题".to_string())?;
    let parent =
        find_topic_mut(root, &parent_id).ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
    let children = ensure_attached_children(parent);
    let index = children
        .iter()
        .position(|child| child.get("id").and_then(Value::as_str) == Some(topic_id))
        .ok_or_else(|| format!("找不到主题 {topic_id}"))?;
    let topic = children.remove(index);
    let mut wrapper = new_topic(title);
    wrapper["children"] = json!({ "attached": [topic] });
    children.insert(index, wrapper);
    Ok(())
}

fn move_sibling(root: &mut Value, topic_id: &str, offset: isize) -> Result<(), String> {
    let parent_id =
        find_parent_id(root, topic_id).ok_or_else(|| "根主题不能调整同级顺序".to_string())?;
    let parent =
        find_topic_mut(root, &parent_id).ok_or_else(|| format!("找不到父主题 {parent_id}"))?;
    let children = ensure_attached_children(parent);
    let index = children
        .iter()
        .position(|child| child.get("id").and_then(Value::as_str) == Some(topic_id))
        .ok_or_else(|| format!("找不到主题 {topic_id}"))?;
    let target = index as isize + offset;
    if target < 0 || target >= children.len() as isize {
        return Ok(());
    }
    children.swap(index, target as usize);
    Ok(())
}

fn assign_new_ids(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("id") {
            object.insert("id".to_string(), Value::String(new_topic_id()));
        }
        if let Some(children) = object
            .get_mut("children")
            .and_then(Value::as_object_mut)
            .and_then(|children| children.get_mut("attached"))
            .and_then(Value::as_array_mut)
        {
            for child in children {
                *child = assign_new_ids(child.clone());
            }
        }
    }
    value
}

fn ensure_attached_children(topic: &mut Value) -> &mut Vec<Value> {
    if !topic.get("children").is_some_and(Value::is_object) {
        topic["children"] = json!({ "attached": [] });
    }
    if !topic["children"]
        .get("attached")
        .is_some_and(Value::is_array)
    {
        topic["children"]["attached"] = json!([]);
    }
    topic["children"]["attached"]
        .as_array_mut()
        .expect("attached children initialized")
}

fn new_topic(title: String) -> Value {
    json!({
        "id": new_topic_id(),
        "class": "topic",
        "title": normalize_title(title),
    })
}

fn normalize_title(title: String) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        "新主题".to_string()
    } else {
        trimmed.to_string()
    }
}

fn new_topic_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:032x}", now ^ u128::from(sequence))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!([
            {
                "id": "sheet",
                "class": "sheet",
                "rootTopic": {
                    "id": "root",
                    "class": "topic",
                    "title": "Root",
                    "unknown": { "keep": true },
                    "children": {
                        "attached": [
                            { "id": "a", "class": "topic", "title": "A", "future": 1 },
                            { "id": "b", "class": "topic", "title": "B" }
                        ]
                    }
                }
            }
        ])
    }

    #[test]
    fn edits_preserve_unknown_fields_and_order() {
        let mut content = fixture();
        apply_edit(
            &mut content,
            MindmapEditDto::AddSibling {
                topic_id: "a".to_string(),
                title: "A2".to_string(),
            },
        )
        .expect("add sibling");
        let topics = &content[0]["rootTopic"]["children"]["attached"];
        assert_eq!(topics.as_array().unwrap().len(), 3);
        assert_eq!(topics[0]["id"], "a");
        assert_eq!(topics[1]["title"], "A2");
        assert_eq!(topics[2]["id"], "b");
        assert_eq!(content[0]["rootTopic"]["unknown"]["keep"], true);
        assert_eq!(topics[0]["future"], 1);
    }

    #[test]
    fn insertion_parenting_and_reordering_preserve_tree() {
        let mut content = fixture();
        apply_edit(
            &mut content,
            MindmapEditDto::InsertSiblingBefore {
                topic_id: "b".to_string(),
                title: "B0".to_string(),
            },
        )
        .expect("insert before");
        let ids: Vec<_> = content[0]["rootTopic"]["children"]["attached"]
            .as_array()
            .unwrap()
            .iter()
            .map(|topic| topic["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids[0], "a");
        assert_eq!(ids[2], "b");

        apply_edit(
            &mut content,
            MindmapEditDto::NewParent {
                topic_id: "a".to_string(),
                title: "Parent of A".to_string(),
            },
        )
        .expect("new parent");
        let parent_id = content[0]["rootTopic"]["children"]["attached"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            content[0]["rootTopic"]["children"]["attached"][0]["title"],
            "Parent of A"
        );
        assert_eq!(
            content[0]["rootTopic"]["children"]["attached"][0]["children"]["attached"][0]["id"],
            "a"
        );

        apply_edit(
            &mut content,
            MindmapEditDto::MoveDown {
                topic_id: parent_id,
            },
        )
        .expect("move down");
        let moved = &content[0]["rootTopic"]["children"]["attached"][1];
        assert_eq!(moved["title"], "Parent of A");
    }

    #[test]
    fn paste_assigns_new_ids_recursively() {
        let mut content = fixture();
        let pasted = content[0]["rootTopic"]["children"]["attached"][0].clone();
        apply_edit(
            &mut content,
            MindmapEditDto::Paste {
                parent_id: "b".to_string(),
                topic_json: serde_json::to_string(&pasted).unwrap(),
            },
        )
        .expect("paste");
        let pasted_topic =
            &content[0]["rootTopic"]["children"]["attached"][1]["children"]["attached"][0];
        assert_ne!(pasted_topic["id"], "a");
        assert_eq!(pasted_topic["future"], 1);
        assert_eq!(pasted_topic["children"]["attached"][0]["id"], Value::Null);
    }

    #[test]
    fn todo_dto_exposes_completion_rendering_state() {
        assert_eq!(
            todo_dto(TodoStatus::InProgress { eighths: 4 }),
            MindmapTodoDto {
                status: MindmapTodoStatusDto::InProgress,
                progress_eighths: Some(4),
                is_complete: false,
            }
        );
        assert_eq!(
            todo_dto(TodoStatus::Done),
            MindmapTodoDto {
                status: MindmapTodoStatusDto::Done,
                progress_eighths: Some(8),
                is_complete: true,
            }
        );
    }

    #[test]
    fn marker_dto_preserves_typed_slots_and_generic_followers() {
        let set = crate::mindmap::TopicMarkerSet {
            entries: vec![
                crate::mindmap::MarkerEntry::Marker(crate::mindmap::TopicMarker {
                    marker_id: "priority-2".to_string(),
                    kind: MarkerKind::Priority(PriorityMarker::Priority2),
                    opaque_fields: serde_json::Map::new(),
                }),
                crate::mindmap::MarkerEntry::Marker(crate::mindmap::TopicMarker {
                    marker_id: "flag-red".to_string(),
                    kind: MarkerKind::Generic {
                        family: MarkerFamily::Flag,
                    },
                    opaque_fields: serde_json::Map::new(),
                }),
            ],
        };
        let dto = topic_markers_dto(&set);
        assert_eq!(dto.priority_marker_id.as_deref(), Some("priority-2"));
        assert_eq!(dto.task_progress_marker_id, None);
        assert_eq!(dto.markers.len(), 2);
        assert_eq!(dto.markers[1].family, "flag");
        assert_eq!(dto.markers[1].label, "Flag Red");
    }

    #[test]
    fn topic_style_dto_preserves_local_text_and_node_values() {
        let dto = topic_style_dto(crate::mindmap::TopicStyle {
            text: crate::mindmap::TopicTextStyle {
                font_family: Some(FontFamily::new("JetBrains Mono").unwrap()),
                font_size: Some(FontSize::points(13.0).unwrap()),
                font_weight: Some(FontWeight::Bold),
                font_style: Some(FontStyle::Italic),
                decoration: Some(crate::mindmap::TextDecoration {
                    underline: true,
                    strike: true,
                }),
                color: Some(StyleColor::new("#112233").unwrap()),
                ..crate::mindmap::TopicTextStyle::default()
            },
            node: crate::mindmap::TopicNodeStyle {
                background: Some(StyleColor::new("#FAFAFA").unwrap()),
                border_color: Some(StyleColor::new("#445566").unwrap()),
                border_width: Some(StyleMeasurement::points(2.0).unwrap()),
                border_pattern: Some(BorderLinePattern::Dash),
                corner_radius: Some(NodeCornerRadius::new(9).unwrap()),
            },
        });
        assert_eq!(dto.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(dto.font_size, Some(13.0));
        assert_eq!(dto.italic, Some(true));
        assert_eq!(dto.underline, Some(true));
        assert_eq!(dto.strike, Some(true));
        assert_eq!(dto.node_fill.as_deref(), Some("#FAFAFA"));
        assert_eq!(dto.border_width, Some(2.0));
        assert_eq!(dto.corner_radius, Some(9));
    }

    #[test]
    fn rename_and_delete_are_scoped_to_requested_topic() {
        let mut content = fixture();
        apply_edit(
            &mut content,
            MindmapEditDto::Rename {
                topic_id: "a".to_string(),
                title: "Renamed".to_string(),
            },
        )
        .expect("rename");
        assert_eq!(
            content[0]["rootTopic"]["children"]["attached"][0]["title"],
            "Renamed"
        );

        apply_edit(
            &mut content,
            MindmapEditDto::Delete {
                topic_id: "a".to_string(),
            },
        )
        .expect("delete");
        let topics = content[0]["rootTopic"]["children"]["attached"]
            .as_array()
            .unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0]["id"], "b");
    }
}
