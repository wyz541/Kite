use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const AGENT_SESSION_SCHEMA_VERSION: u32 = 1;
const MAX_AGENT_SESSION_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AGENT_CONVERSATIONS: usize = 16;
const MAX_AGENT_MESSAGES: usize = 2_000;
const MAX_AGENT_MESSAGE_BYTES: usize = 512 * 1024;
const MAX_AGENT_CONTEXT_PATHS: usize = 128;

static SAVE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageRole {
    #[default]
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentMessage {
    pub id: String,
    pub role: AgentMessageRole,
    pub content: String,
    pub unix_ms: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentConversation {
    pub id: String,
    pub title: String,
    pub profile_id: String,
    pub model: Option<String>,
    pub working_directory: String,
    pub context_paths: Vec<String>,
    pub follow_selection: bool,
    pub messages: Vec<AgentMessage>,
    pub remote_session_id: Option<String>,
    pub draft: String,
    pub window_width: u32,
    pub window_height: u32,
    pub updated_unix_ms: u64,
}

impl AgentConversation {
    pub fn new(
        id: impl Into<String>,
        profile_id: impl Into<String>,
        working_directory: impl Into<String>,
        context_paths: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: "新的 Agent 对话".to_string(),
            profile_id: profile_id.into(),
            model: None,
            working_directory: working_directory.into(),
            context_paths,
            follow_selection: false,
            messages: Vec::new(),
            remote_session_id: None,
            draft: String::new(),
            window_width: 960,
            window_height: 760,
            updated_unix_ms: unix_ms(),
        }
    }

    pub fn missing_paths(&self) -> Vec<String> {
        self.context_paths
            .iter()
            .filter(|path| !Path::new(path).exists())
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentSessionStore {
    pub schema_version: u32,
    pub active_conversation_id: Option<String>,
    pub conversations: Vec<AgentConversation>,
}

impl Default for AgentSessionStore {
    fn default() -> Self {
        Self {
            schema_version: AGENT_SESSION_SCHEMA_VERSION,
            active_conversation_id: None,
            conversations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentSessionRestore {
    pub store: AgentSessionStore,
    pub session_path: String,
    pub missing_paths: Vec<String>,
    pub profile_missing: bool,
    pub working_directory_missing: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentContextRepair {
    pub conversation_id: String,
    pub old_path: String,
    pub replacement_path: String,
}

pub fn agent_session_path() -> PathBuf {
    if let Some(path) = std::env::var_os("KITE_AGENT_SESSION_PATH") {
        return PathBuf::from(path);
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Kite")
        .join("agent-session.json")
}

pub fn load_agent_sessions() -> AgentSessionRestore {
    load_agent_sessions_at(&agent_session_path())
}

pub fn load_agent_sessions_at(path: &Path) -> AgentSessionRestore {
    let mut warning = None;
    let store = load_store_file(path, &mut warning)
        .or_else(|| load_store_file(&backup_path(path), &mut warning))
        .unwrap_or_default();
    let profile_ids = crate::agent::load_agent_profiles()
        .store
        .profiles
        .into_iter()
        .map(|profile| profile.id)
        .collect::<Vec<_>>();
    let active = store
        .active_conversation_id
        .as_deref()
        .and_then(|id| {
            store
                .conversations
                .iter()
                .find(|conversation| conversation.id == id)
        })
        .or_else(|| store.conversations.first());
    let missing_paths = active
        .map(AgentConversation::missing_paths)
        .unwrap_or_default();
    let profile_missing = active.is_some_and(|conversation| {
        !profile_ids.is_empty() && !profile_ids.iter().any(|id| id == &conversation.profile_id)
    });
    let working_directory_missing =
        active.is_some_and(|conversation| !Path::new(&conversation.working_directory).is_dir());
    AgentSessionRestore {
        store,
        session_path: path.to_string_lossy().into_owned(),
        missing_paths,
        profile_missing,
        working_directory_missing,
        warning,
    }
}

pub fn load_agent_conversation(id: &str) -> Option<AgentConversation> {
    load_agent_sessions()
        .store
        .conversations
        .into_iter()
        .find(|conversation| conversation.id == id)
}

pub fn save_agent_conversation(
    conversation: AgentConversation,
) -> Result<AgentSessionStore, String> {
    let path = agent_session_path();
    let restore = load_agent_sessions_at(&path);
    let mut store = restore.store;
    store.active_conversation_id = Some(conversation.id.clone());
    if let Some(existing) = store
        .conversations
        .iter_mut()
        .find(|existing| existing.id == conversation.id)
    {
        *existing = conversation;
    } else {
        store.conversations.push(conversation);
    }
    save_agent_sessions_at(&path, store)
}

pub fn delete_agent_conversation(id: &str) -> Result<AgentSessionStore, String> {
    let path = agent_session_path();
    let mut store = load_agent_sessions_at(&path).store;
    store
        .conversations
        .retain(|conversation| conversation.id != id);
    if store.active_conversation_id.as_deref() == Some(id) {
        store.active_conversation_id = store
            .conversations
            .iter()
            .max_by_key(|conversation| conversation.updated_unix_ms)
            .map(|conversation| conversation.id.clone());
    }
    save_agent_sessions_at(&path, store)
}

pub fn repair_agent_context(repair: AgentContextRepair) -> Result<AgentSessionStore, String> {
    if !Path::new(&repair.replacement_path).exists() {
        return Err("替代路径不存在".to_string());
    }
    let path = agent_session_path();
    let mut store = load_agent_sessions_at(&path).store;
    let conversation = store
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == repair.conversation_id)
        .ok_or_else(|| "找不到要修复的 Agent 会话".to_string())?;
    for context_path in &mut conversation.context_paths {
        if *context_path == repair.old_path {
            *context_path = repair.replacement_path.clone();
        }
    }
    if conversation.working_directory == repair.old_path {
        conversation.working_directory = repair.replacement_path;
    }
    conversation.updated_unix_ms = unix_ms();
    save_agent_sessions_at(&path, store)
}

pub fn save_agent_sessions_at(
    path: &Path,
    mut store: AgentSessionStore,
) -> Result<AgentSessionStore, String> {
    let _guard = SAVE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Agent 会话保存锁不可用".to_string())?;
    store = normalize_store(store)?;
    let bytes = serde_json::to_vec_pretty(&store)
        .map_err(|error| format!("无法序列化 Agent 会话：{error}"))?;
    if bytes.len() as u64 > MAX_AGENT_SESSION_BYTES {
        return Err("Agent 会话超过 8 MiB 上限".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 Agent 会话目录：{error}"))?;
    }
    let temporary = temporary_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建 Agent 临时会话：{error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("无法写入 Agent 临时会话：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法同步 Agent 临时会话：{error}"))?;
    }
    let backup = backup_path(path);
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| format!("无法替换 Agent 会话备份：{error}"))?;
        }
        fs::rename(path, &backup).map_err(|error| format!("无法轮换 Agent 会话：{error}"))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法替换 Agent 会话：{error}"));
    }
    Ok(store)
}

pub fn new_agent_conversation_id() -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("agent-{}-{}-{sequence}", std::process::id(), unix_ms())
}

fn load_store_file(path: &Path, warning: &mut Option<String>) -> Option<AgentSessionStore> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_AGENT_SESSION_BYTES {
        *warning = Some("Agent 会话文件过大，已忽略".to_string());
        return None;
    }
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice::<AgentSessionStore>(&bytes) {
        Ok(store) => match normalize_store(store) {
            Ok(store) => Some(store),
            Err(error) => {
                *warning = Some(format!("Agent 会话校验失败：{error}"));
                None
            }
        },
        Err(error) => {
            *warning = Some(format!("Agent 会话损坏，已使用安全默认值：{error}"));
            None
        }
    }
}

fn normalize_store(mut store: AgentSessionStore) -> Result<AgentSessionStore, String> {
    if store.schema_version != AGENT_SESSION_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 Agent session schema_version：{}",
            store.schema_version
        ));
    }
    if store.conversations.len() > MAX_AGENT_CONVERSATIONS {
        store
            .conversations
            .sort_by_key(|conversation| std::cmp::Reverse(conversation.updated_unix_ms));
        store.conversations.truncate(MAX_AGENT_CONVERSATIONS);
    }
    for conversation in &mut store.conversations {
        if conversation.id.trim().is_empty() || conversation.profile_id.trim().is_empty() {
            return Err("Agent 会话缺少标识或 Agent profile".to_string());
        }
        if conversation.working_directory.trim().is_empty() {
            return Err("Agent 会话缺少工作目录".to_string());
        }
        conversation.messages.retain(|message| {
            !message.id.trim().is_empty()
                && !message.content.is_empty()
                && message.content.len() <= MAX_AGENT_MESSAGE_BYTES
        });
        if conversation.messages.len() > MAX_AGENT_MESSAGES {
            conversation
                .messages
                .drain(..conversation.messages.len() - MAX_AGENT_MESSAGES);
        }
        conversation
            .context_paths
            .retain(|path| !path.trim().is_empty());
        conversation.context_paths.truncate(MAX_AGENT_CONTEXT_PATHS);
        let mut seen_paths = std::collections::HashSet::new();
        conversation
            .context_paths
            .retain(|path| seen_paths.insert(path.clone()));
        conversation.window_width = conversation.window_width.clamp(680, 3_840);
        conversation.window_height = conversation.window_height.clamp(520, 2_160);
        if conversation.title.trim().is_empty() {
            conversation.title = conversation
                .messages
                .iter()
                .find(|message| message.role == AgentMessageRole::User)
                .map(|message| {
                    message
                        .content
                        .lines()
                        .next()
                        .unwrap_or("Agent 对话")
                        .chars()
                        .take(48)
                        .collect()
                })
                .unwrap_or_else(|| "Agent 对话".to_string());
        }
    }
    if !store.active_conversation_id.as_ref().is_some_and(|id| {
        store
            .conversations
            .iter()
            .any(|conversation| &conversation.id == id)
    }) {
        store.active_conversation_id = store
            .conversations
            .iter()
            .max_by_key(|conversation| conversation.updated_unix_ms)
            .map(|conversation| conversation.id.clone());
    }
    Ok(store)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "kite-agent-session-{label}-{}-{unique}",
                std::process::id()
            ))
            .join("agent-session.json")
    }

    #[test]
    fn conversation_round_trips_and_preserves_missing_paths() {
        let path = temp_path("roundtrip");
        let mut conversation = AgentConversation::new(
            "conversation-1",
            "profile-1",
            std::env::temp_dir().to_string_lossy().into_owned(),
            vec![
                std::env::temp_dir().to_string_lossy().into_owned(),
                "/path/that/does/not/exist".to_string(),
            ],
        );
        conversation.messages.push(AgentMessage {
            id: "message-1".to_string(),
            role: AgentMessageRole::User,
            content: "hello".to_string(),
            unix_ms: unix_ms(),
            truncated: false,
        });
        let store = AgentSessionStore {
            schema_version: AGENT_SESSION_SCHEMA_VERSION,
            active_conversation_id: Some(conversation.id.clone()),
            conversations: vec![conversation],
        };
        save_agent_sessions_at(&path, store.clone()).expect("save");
        let loaded = load_store_file(&path, &mut None).expect("load");
        assert_eq!(loaded, store);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn repair_replaces_a_missing_context_path() {
        let conversation = AgentConversation::new(
            "conversation-1",
            "profile-1",
            std::env::temp_dir().to_string_lossy().into_owned(),
            vec!["/missing".to_string()],
        );
        let mut store = AgentSessionStore::default();
        store.active_conversation_id = Some(conversation.id.clone());
        store.conversations.push(conversation);
        let repaired = normalize_store(store).expect("valid");
        let replacement = std::env::temp_dir().to_string_lossy().into_owned();
        let mut conversation = repaired.conversations[0].clone();
        conversation.context_paths = vec![replacement.clone()];
        assert_eq!(conversation.context_paths, vec![replacement]);
    }
}
