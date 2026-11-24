use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    AGENT_PROFILE_SCHEMA_VERSION, AgentConfigError, AgentConfigWarning, AgentProfile,
    AgentProfileSnapshot, MAX_AGENT_PROFILE_FILE_BYTES, MAX_AGENT_PROFILES, prepare_loaded_profile,
    prepare_new_profile,
};

const AGENT_CONFIG_ENV: &str = "KITE_AGENT_PROFILE_PATH";

static AGENT_CONFIG_IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AgentConfigFile {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    profiles: Vec<AgentProfile>,
    #[serde(default)]
    default_profile_id: Option<String>,
}

impl Default for AgentConfigFile {
    fn default() -> Self {
        Self {
            schema_version: AGENT_PROFILE_SCHEMA_VERSION,
            profiles: Vec::new(),
            default_profile_id: None,
        }
    }
}

#[derive(Debug, uniffi::Object)]
pub struct AgentProfileStore {
    path: Option<PathBuf>,
}

#[uniffi::export]
impl AgentProfileStore {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            path: default_agent_profile_path(),
        })
    }

    #[uniffi::constructor(name = "with_path")]
    pub fn with_path(path: String) -> Arc<Self> {
        Arc::new(Self {
            path: Some(PathBuf::from(path)),
        })
    }

    pub fn config_path(&self) -> Option<String> {
        self.path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
    }

    /// Loads the store without surfacing a startup-blocking error. Invalid files
    /// yield an empty snapshot plus a typed warning so the UI can offer recovery.
    pub fn snapshot(&self) -> AgentProfileSnapshot {
        let _guard = lock_config_io();
        match self.load_config_locked() {
            Ok((config, warning)) => snapshot_from_config(config, warning),
            Err(error) => snapshot_from_error(&error),
        }
    }

    pub fn list_profiles(&self) -> Result<Vec<AgentProfile>, AgentConfigError> {
        let _guard = lock_config_io();
        let (config, _) = self.load_config_locked()?;
        Ok(config.profiles)
    }

    pub fn get_profile(
        &self,
        profile_id: String,
    ) -> Result<Option<AgentProfile>, AgentConfigError> {
        let _guard = lock_config_io();
        let (config, _) = self.load_config_locked()?;
        let profile_id = profile_id.trim();
        Ok(config
            .profiles
            .into_iter()
            .find(|profile| profile.id == profile_id))
    }

    pub fn default_profile(&self) -> Result<Option<AgentProfile>, AgentConfigError> {
        let _guard = lock_config_io();
        let (config, _) = self.load_config_locked()?;
        let Some(default_profile_id) = config.default_profile_id else {
            return Ok(None);
        };
        Ok(config
            .profiles
            .into_iter()
            .find(|profile| profile.id == default_profile_id))
    }

    pub fn create_profile(&self, profile: AgentProfile) -> Result<AgentProfile, AgentConfigError> {
        let _guard = lock_config_io();
        let (mut config, _) = self.load_config_locked()?;
        if config.profiles.len() >= MAX_AGENT_PROFILES {
            return Err(AgentConfigError::ProfileLimitReached {
                limit: MAX_AGENT_PROFILES as u32,
            });
        }
        let profile = prepare_new_profile(profile)?;
        if config
            .profiles
            .iter()
            .any(|existing| existing.id == profile.id)
        {
            return Err(AgentConfigError::DuplicateProfileId {
                profile_id: profile.id,
            });
        }
        if config
            .profiles
            .iter()
            .any(|existing| existing.name.to_lowercase() == profile.name.to_lowercase())
        {
            return Err(AgentConfigError::DuplicateProfileName { name: profile.name });
        }
        config.profiles.push(profile.clone());
        config = normalize_config(config)?;
        self.save_config_locked(&config)?;
        Ok(profile)
    }

    pub fn update_profile(&self, profile: AgentProfile) -> Result<AgentProfile, AgentConfigError> {
        let _guard = lock_config_io();
        let (mut config, _) = self.load_config_locked()?;
        let profile = prepare_loaded_profile(profile, false)?;
        let Some(index) = config
            .profiles
            .iter()
            .position(|existing| existing.id == profile.id)
        else {
            return Err(AgentConfigError::ProfileNotFound {
                profile_id: profile.id,
            });
        };
        if config
            .profiles
            .iter()
            .enumerate()
            .any(|(existing_index, existing)| {
                existing_index != index
                    && existing.name.to_lowercase() == profile.name.to_lowercase()
            })
        {
            return Err(AgentConfigError::DuplicateProfileName { name: profile.name });
        }
        config.profiles[index] = profile.clone();
        config = normalize_config(config)?;
        self.save_config_locked(&config)?;
        Ok(profile)
    }

    pub fn delete_profile(&self, profile_id: String) -> Result<bool, AgentConfigError> {
        let _guard = lock_config_io();
        let (mut config, _) = self.load_config_locked()?;
        let profile_id = profile_id.trim().to_string();
        let original_len = config.profiles.len();
        config.profiles.retain(|profile| profile.id != profile_id);
        if config.profiles.len() == original_len {
            return Ok(false);
        }
        config = normalize_config(config)?;
        self.save_config_locked(&config)?;
        Ok(true)
    }

    pub fn set_default_profile(
        &self,
        profile_id: String,
    ) -> Result<AgentProfileSnapshot, AgentConfigError> {
        let _guard = lock_config_io();
        let (mut config, _) = self.load_config_locked()?;
        let profile_id = profile_id.trim().to_string();
        if !config
            .profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            return Err(AgentConfigError::ProfileNotFound { profile_id });
        }
        config.default_profile_id = Some(profile_id);
        config = normalize_config(config)?;
        self.save_config_locked(&config)?;
        Ok(snapshot_from_config(config, None))
    }

    /// Explicitly replaces an unreadable or future-schema config with an empty
    /// supported config. Call this only after the user confirms recovery.
    pub fn reset(&self) -> Result<AgentProfileSnapshot, AgentConfigError> {
        let _guard = lock_config_io();
        let config = AgentConfigFile::default();
        self.save_config_locked(&config)?;
        Ok(snapshot_from_config(config, None))
    }
}

impl AgentProfileStore {
    pub(crate) fn replace_snapshot(
        &self,
        snapshot: AgentProfileSnapshot,
    ) -> Result<AgentProfileSnapshot, AgentConfigError> {
        let _guard = lock_config_io();
        let config = normalize_config(AgentConfigFile {
            schema_version: snapshot.schema_version,
            profiles: snapshot.profiles,
            default_profile_id: snapshot.default_profile_id,
        })?;
        self.save_config_locked(&config)?;
        Ok(snapshot_from_config(config, None))
    }

    fn load_config_locked(
        &self,
    ) -> Result<(AgentConfigFile, Option<AgentConfigWarning>), AgentConfigError> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| AgentConfigError::StorageUnavailable {
                message: "cannot resolve the agent config directory".to_string(),
            })?;
        load_config_at(path)
    }

    fn save_config_locked(&self, config: &AgentConfigFile) -> Result<(), AgentConfigError> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| AgentConfigError::StorageUnavailable {
                message: "cannot resolve the agent config directory".to_string(),
            })?;
        let config = normalize_config(config.clone())?;
        write_config_at(path, &config)
    }
}

fn default_agent_profile_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(AGENT_CONFIG_ENV)
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .map(|path| path.join("Kite").join("agent.json"))
}

fn lock_config_io() -> MutexGuard<'static, ()> {
    AGENT_CONFIG_IO_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn snapshot_from_config(
    config: AgentConfigFile,
    warning: Option<AgentConfigWarning>,
) -> AgentProfileSnapshot {
    AgentProfileSnapshot {
        schema_version: AGENT_PROFILE_SCHEMA_VERSION,
        profiles: config.profiles,
        default_profile_id: config.default_profile_id,
        warning,
    }
}

fn snapshot_from_error(error: &AgentConfigError) -> AgentProfileSnapshot {
    AgentProfileSnapshot {
        schema_version: AGENT_PROFILE_SCHEMA_VERSION,
        profiles: Vec::new(),
        default_profile_id: None,
        warning: Some(error.warning()),
    }
}

fn load_config_at(
    path: &Path,
) -> Result<(AgentConfigFile, Option<AgentConfigWarning>), AgentConfigError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((AgentConfigFile::default(), None));
        }
        Err(error) => {
            return Err(AgentConfigError::Io {
                message: error.to_string(),
            });
        }
    };
    if !metadata.is_file() {
        return Err(AgentConfigError::CorruptConfig {
            message: "agent config path is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_AGENT_PROFILE_FILE_BYTES {
        return Err(AgentConfigError::ConfigTooLarge {
            bytes: metadata.len(),
            max_bytes: MAX_AGENT_PROFILE_FILE_BYTES,
        });
    }
    let bytes = fs::read(path).map_err(|error| AgentConfigError::Io {
        message: error.to_string(),
    })?;
    if bytes.len() as u64 > MAX_AGENT_PROFILE_FILE_BYTES {
        return Err(AgentConfigError::ConfigTooLarge {
            bytes: bytes.len() as u64,
            max_bytes: MAX_AGENT_PROFILE_FILE_BYTES,
        });
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        AgentConfigError::CorruptConfig {
            message: error.to_string(),
        }
    })?;
    let source_schema_version = schema_version_from_value(&value)?;

    if source_schema_version > AGENT_PROFILE_SCHEMA_VERSION {
        return Err(AgentConfigError::UnknownSchemaVersion {
            found: source_schema_version,
            supported: AGENT_PROFILE_SCHEMA_VERSION,
        });
    }

    let (config, migrated_from) = if source_schema_version == 0 {
        (migrate_v0_config(value)?, Some(0))
    } else if source_schema_version == AGENT_PROFILE_SCHEMA_VERSION {
        let config = serde_json::from_value::<AgentConfigFile>(value).map_err(|error| {
            AgentConfigError::CorruptConfig {
                message: error.to_string(),
            }
        })?;
        (normalize_config(config)?, None)
    } else {
        return Err(AgentConfigError::UnknownSchemaVersion {
            found: source_schema_version,
            supported: AGENT_PROFILE_SCHEMA_VERSION,
        });
    };

    let warning = migrated_from.map(|from_version| match write_config_at(path, &config) {
        Ok(()) => AgentConfigWarning::MigrationApplied {
            from_version,
            to_version: AGENT_PROFILE_SCHEMA_VERSION,
        },
        Err(error) => error.warning(),
    });
    Ok((config, warning))
}

fn schema_version_from_value(value: &Value) -> Result<u32, AgentConfigError> {
    if value.is_array() {
        return Ok(0);
    }
    let Value::Object(object) = value else {
        return Err(AgentConfigError::CorruptConfig {
            message: "agent config root must be an object or a legacy profile array".to_string(),
        });
    };
    let Some(schema_version) = object.get("schema_version") else {
        return Ok(0);
    };
    let Some(schema_version) = schema_version.as_u64() else {
        return Err(AgentConfigError::CorruptConfig {
            message: "schema_version must be a non-negative integer".to_string(),
        });
    };
    u32::try_from(schema_version).map_err(|_| AgentConfigError::CorruptConfig {
        message: "schema_version is out of range".to_string(),
    })
}

fn migrate_v0_config(value: Value) -> Result<AgentConfigFile, AgentConfigError> {
    let (profiles_value, default_profile_id) = match value {
        Value::Array(profiles) => (Value::Array(profiles), None),
        Value::Object(mut object) => {
            let profiles =
                object
                    .remove("profiles")
                    .ok_or_else(|| AgentConfigError::CorruptConfig {
                        message: "legacy agent config is missing profiles".to_string(),
                    })?;
            let default_profile_id = object
                .remove("default_profile_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned));
            (profiles, default_profile_id)
        }
        _ => {
            return Err(AgentConfigError::CorruptConfig {
                message: "unsupported legacy agent config shape".to_string(),
            });
        }
    };
    let profiles =
        serde_json::from_value::<Vec<AgentProfile>>(profiles_value).map_err(|error| {
            AgentConfigError::CorruptConfig {
                message: error.to_string(),
            }
        })?;
    let profiles = profiles
        .into_iter()
        .map(|profile| prepare_loaded_profile(profile, true))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_config(AgentConfigFile {
        schema_version: AGENT_PROFILE_SCHEMA_VERSION,
        profiles,
        default_profile_id,
    })
}

fn normalize_config(mut config: AgentConfigFile) -> Result<AgentConfigFile, AgentConfigError> {
    if config.schema_version != AGENT_PROFILE_SCHEMA_VERSION {
        return Err(AgentConfigError::UnknownSchemaVersion {
            found: config.schema_version,
            supported: AGENT_PROFILE_SCHEMA_VERSION,
        });
    }
    if config.profiles.len() > MAX_AGENT_PROFILES {
        return Err(AgentConfigError::ProfileLimitReached {
            limit: MAX_AGENT_PROFILES as u32,
        });
    }

    let mut profile_ids = HashSet::new();
    let mut profile_names = HashSet::new();
    for profile in &mut config.profiles {
        super::validate_and_normalize_profile(profile)?;
        if !profile_ids.insert(profile.id.clone()) {
            return Err(AgentConfigError::DuplicateProfileId {
                profile_id: profile.id.clone(),
            });
        }
        if !profile_names.insert(profile.name.to_lowercase()) {
            return Err(AgentConfigError::DuplicateProfileName {
                name: profile.name.clone(),
            });
        }
    }

    let requested_default = config
        .default_profile_id
        .take()
        .map(|profile_id| profile_id.trim().to_string())
        .filter(|profile_id| !profile_id.is_empty());
    config.default_profile_id = requested_default
        .filter(|profile_id| {
            config
                .profiles
                .iter()
                .any(|profile| profile.id == *profile_id)
        })
        .or_else(|| config.profiles.first().map(|profile| profile.id.clone()));
    Ok(config)
}

fn write_config_at(path: &Path, config: &AgentConfigFile) -> Result<(), AgentConfigError> {
    let bytes =
        serde_json::to_vec_pretty(config).map_err(|error| AgentConfigError::CorruptConfig {
            message: format!("cannot serialize agent config: {error}"),
        })?;
    if bytes.len() as u64 > MAX_AGENT_PROFILE_FILE_BYTES {
        return Err(AgentConfigError::ConfigTooLarge {
            bytes: bytes.len() as u64,
            max_bytes: MAX_AGENT_PROFILE_FILE_BYTES,
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| AgentConfigError::Io {
            message: error.to_string(),
        })?;
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(&bytes))
        .map_err(|error| AgentConfigError::Io {
            message: error.to_string(),
        })
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
