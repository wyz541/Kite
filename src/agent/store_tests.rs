use super::*;
use crate::agent::{
    AgentEnvironmentVariable, AgentTransport, MAX_AGENT_PROFILE_FILE_BYTES, PermissionPolicy,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kite-agent-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self { path }
    }

    fn config_path(&self) -> PathBuf {
        self.path.join("agent.json")
    }

    fn path_string(&self) -> String {
        self.config_path().to_string_lossy().into_owned()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn profile(name: &str) -> AgentProfile {
    AgentProfile {
        name: name.to_string(),
        executable: Some("agent".to_string()),
        credential_ref: Some("keychain:kite/agent/default".to_string()),
        ..AgentProfile::default()
    }
}

#[test]
fn creates_updates_defaults_and_deletes_profiles() {
    let temp = TestDirectory::new("crud");
    let store = AgentProfileStore::with_path(temp.path_string());

    let snapshot = store.snapshot();
    assert!(snapshot.profiles.is_empty());
    assert!(snapshot.default_profile_id.is_none());
    assert!(snapshot.warning.is_none());

    let first = store
        .create_profile(profile("Local Agent"))
        .expect("create first profile");
    assert!(!first.id.is_empty());
    let first_snapshot = store.snapshot();
    assert_eq!(first_snapshot.profiles.len(), 1);
    assert_eq!(
        first_snapshot.default_profile_id.as_deref(),
        Some(first.id.as_str())
    );

    let second = store
        .create_profile(profile("Review Agent"))
        .expect("create second profile");
    let second_snapshot = store.snapshot();
    assert_eq!(second_snapshot.profiles.len(), 2);
    assert_eq!(
        second_snapshot.default_profile_id.as_deref(),
        Some(first.id.as_str()),
        "creating another profile must not change the default"
    );

    let mut updated = first.clone();
    updated.name = "Local Coding Agent".to_string();
    updated.permission = PermissionPolicy::ConfirmWrites;
    let updated = store.update_profile(updated).expect("update profile");
    assert_eq!(updated.name, "Local Coding Agent");
    assert_eq!(
        store
            .get_profile(first.id.clone())
            .expect("get updated profile")
            .expect("profile exists")
            .name,
        "Local Coding Agent"
    );

    let defaulted = store
        .set_default_profile(second.id.clone())
        .expect("set default profile");
    assert_eq!(
        defaulted.default_profile_id.as_deref(),
        Some(second.id.as_str())
    );
    assert_eq!(
        store
            .default_profile()
            .expect("load default profile")
            .expect("default exists")
            .id,
        second.id
    );

    assert!(
        store
            .delete_profile(second.id.clone())
            .expect("delete default profile")
    );
    let after_delete = store.snapshot();
    assert_eq!(after_delete.profiles.len(), 1);
    assert_eq!(
        after_delete.default_profile_id.as_deref(),
        Some(first.id.as_str())
    );
    assert!(
        store
            .delete_profile(first.id.clone())
            .expect("delete last profile")
    );
    let empty = store.snapshot();
    assert!(empty.profiles.is_empty());
    assert!(empty.default_profile_id.is_none());
}

#[test]
fn enforces_unique_names_and_ids_case_insensitively_for_names() {
    let temp = TestDirectory::new("duplicates");
    let store = AgentProfileStore::with_path(temp.path_string());
    let first = store
        .create_profile(profile("Local Agent"))
        .expect("create first profile");

    let duplicate_name = store
        .create_profile(profile("local agent"))
        .expect_err("duplicate names should fail");
    assert!(matches!(
        duplicate_name,
        AgentConfigError::DuplicateProfileName { .. }
    ));

    let mut duplicate_id = profile("Another Agent");
    duplicate_id.id = first.id;
    let duplicate_id = store
        .create_profile(duplicate_id)
        .expect_err("duplicate ids should fail");
    assert!(matches!(
        duplicate_id,
        AgentConfigError::DuplicateProfileId { .. }
    ));
}

#[test]
fn validates_profile_fields_before_writing() {
    let temp = TestDirectory::new("validation");
    let store = AgentProfileStore::with_path(temp.path_string());

    let mut missing_executable = profile("Missing Executable");
    missing_executable.executable = None;
    assert!(matches!(
        store.create_profile(missing_executable),
        Err(AgentConfigError::InvalidProfile { field, .. }) if field == "executable"
    ));

    let mut invalid_timeout = profile("Invalid Timeout");
    invalid_timeout.timeout_seconds = 0;
    assert!(matches!(
        store.create_profile(invalid_timeout),
        Err(AgentConfigError::InvalidProfile { field, .. }) if field == "timeout_seconds"
    ));

    let mut invalid_http = profile("Invalid HTTP");
    invalid_http.transport = AgentTransport::AcpHttp;
    invalid_http.executable = None;
    invalid_http.endpoint = Some("file:///tmp/agent".to_string());
    assert!(matches!(
        store.create_profile(invalid_http),
        Err(AgentConfigError::InvalidProfile { field, .. }) if field == "endpoint"
    ));
    assert!(!temp.config_path().exists());
}

#[test]
fn persists_only_safe_configuration_and_credential_references() {
    let temp = TestDirectory::new("secrets");
    let store = AgentProfileStore::with_path(temp.path_string());
    let mut safe = profile("Safe Agent");
    safe.environment = vec![AgentEnvironmentVariable {
        name: "LANG".to_string(),
        value: "en_US.UTF-8".to_string(),
    }];
    safe.arguments = vec!["--read-only".to_string()];
    store.create_profile(safe).expect("create safe profile");

    let before_attempt = fs::read(temp.config_path()).expect("read safe config");
    let before_text = String::from_utf8(before_attempt.clone()).expect("utf8 config");
    assert!(before_text.contains("\"credential_ref\": \"keychain:kite/agent/default\""));

    let mut sensitive = profile("Sensitive Agent");
    sensitive.environment = vec![AgentEnvironmentVariable {
        name: "OPENAI_API_KEY".to_string(),
        value: "not-stored-secret".to_string(),
    }];
    let error = store
        .create_profile(sensitive)
        .expect_err("sensitive environment must fail");
    assert!(error.to_string().contains("credential_ref"));
    assert!(!error.to_string().contains("not-stored-secret"));

    let after_attempt = fs::read(temp.config_path()).expect("read unchanged config");
    assert_eq!(before_attempt, after_attempt);
    let after_text = String::from_utf8(after_attempt).expect("utf8 config");
    assert!(!after_text.contains("not-stored-secret"));
    assert!(!after_text.contains("OPENAI_API_KEY"));
}

#[test]
fn corrupt_future_and_oversized_configs_fall_back_without_blocking_startup() {
    let temp = TestDirectory::new("fallback");
    let store = AgentProfileStore::with_path(temp.path_string());

    fs::write(temp.config_path(), b"{not json").expect("write corrupt config");
    let corrupt = store.snapshot();
    assert!(corrupt.profiles.is_empty());
    assert!(matches!(
        corrupt.warning,
        Some(AgentConfigWarning::CorruptConfig { .. })
    ));
    assert!(matches!(
        store.list_profiles(),
        Err(AgentConfigError::CorruptConfig { .. })
    ));

    fs::write(
        temp.config_path(),
        br#"{"schema_version":99,"profiles":[]}"#,
    )
    .expect("write future config");
    let future = store.snapshot();
    assert!(matches!(
        future.warning,
        Some(AgentConfigWarning::UnknownSchemaVersion {
            found: 99,
            supported: AGENT_PROFILE_SCHEMA_VERSION,
        })
    ));
    assert!(matches!(
        store.list_profiles(),
        Err(AgentConfigError::UnknownSchemaVersion { found: 99, .. })
    ));

    fs::write(
        temp.config_path(),
        vec![b'x'; MAX_AGENT_PROFILE_FILE_BYTES as usize + 1],
    )
    .expect("write oversized config");
    let oversized = store.snapshot();
    assert!(matches!(
        oversized.warning,
        Some(AgentConfigWarning::ConfigTooLarge { .. })
    ));

    let reset = store.reset().expect("reset invalid config");
    assert!(reset.profiles.is_empty());
    assert!(reset.warning.is_none());
    let persisted: Value =
        serde_json::from_slice(&fs::read(temp.config_path()).expect("read reset config"))
            .expect("parse reset config");
    assert_eq!(persisted["schema_version"], AGENT_PROFILE_SCHEMA_VERSION);
}

#[test]
fn migrates_legacy_profile_arrays_and_persists_the_new_schema() {
    let temp = TestDirectory::new("migration");
    let store = AgentProfileStore::with_path(temp.path_string());
    fs::write(
        temp.config_path(),
        serde_json::to_vec(&serde_json::json!([{
            "name": "Legacy Agent",
            "command": "legacy-agent",
            "timeout_seconds": 30
        }]))
        .expect("serialize legacy config"),
    )
    .expect("write legacy config");

    let snapshot = store.snapshot();
    assert!(matches!(
        snapshot.warning,
        Some(AgentConfigWarning::MigrationApplied {
            from_version: 0,
            to_version: AGENT_PROFILE_SCHEMA_VERSION,
        })
    ));
    assert_eq!(snapshot.schema_version, AGENT_PROFILE_SCHEMA_VERSION);
    assert_eq!(snapshot.profiles.len(), 1);
    assert_eq!(snapshot.profiles[0].name, "Legacy Agent");
    assert_eq!(
        snapshot.profiles[0].executable.as_deref(),
        Some("legacy-agent")
    );
    assert!(!snapshot.profiles[0].id.is_empty());
    assert_eq!(
        snapshot.default_profile_id.as_deref(),
        Some(snapshot.profiles[0].id.as_str())
    );

    let persisted: Value =
        serde_json::from_slice(&fs::read(temp.config_path()).expect("read migrated config"))
            .expect("parse migrated config");
    assert_eq!(persisted["schema_version"], AGENT_PROFILE_SCHEMA_VERSION);
    assert_eq!(persisted["profiles"][0]["executable"], "legacy-agent");
    assert!(store.snapshot().warning.is_none());
}

#[test]
fn concurrent_creates_from_independent_store_instances_do_not_lose_profiles() {
    let temp = TestDirectory::new("concurrent");
    let path = Arc::new(temp.path_string());
    let worker_count = 16;
    let barrier = Arc::new(Barrier::new(worker_count));
    let mut handles = Vec::new();

    for index in 0..worker_count {
        let path = Arc::clone(&path);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let store = AgentProfileStore::with_path((*path).clone());
            barrier.wait();
            store
                .create_profile(profile(&format!("Agent {index}")))
                .expect("concurrent create");
        }));
    }
    for handle in handles {
        handle.join().expect("join concurrent writer");
    }

    let store = AgentProfileStore::with_path((*path).clone());
    let snapshot = store.snapshot();
    assert!(snapshot.warning.is_none());
    assert_eq!(snapshot.profiles.len(), worker_count);
    assert!(snapshot.default_profile_id.is_some());

    let persisted: Value =
        serde_json::from_slice(&fs::read(temp.config_path()).expect("read concurrent config"))
            .expect("parse concurrent config");
    assert_eq!(
        persisted["profiles"]
            .as_array()
            .expect("profiles array")
            .len(),
        worker_count
    );
}
