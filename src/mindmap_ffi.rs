use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::ffi::KiteError;
use crate::mindmap::XmindPackage;

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
