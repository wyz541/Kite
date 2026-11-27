use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::ffi::KiteError;
use crate::mindmap::XmindPackage;
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
