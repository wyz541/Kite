const KEYCHAIN_SERVICE: &str = "com.kite.agent";

pub fn credential_ref(profile_id: &str) -> String {
    format!("keychain://{KEYCHAIN_SERVICE}/{profile_id}")
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn entry(profile_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, profile_id)
        .map_err(|error| format!("无法访问系统钥匙串：{error}"))
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn save_secret(profile_id: &str, secret: &str) -> Result<String, String> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Err("凭据不能为空".to_string());
    }
    if secret.len() > 256 * 1024 {
        return Err("凭据过长".to_string());
    }
    entry(profile_id)?
        .set_password(secret)
        .map_err(|error| format!("无法把凭据保存到系统钥匙串：{error}"))?;
    Ok(credential_ref(profile_id))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn save_secret(_profile_id: &str, _secret: &str) -> Result<String, String> {
    Err("当前平台没有可用的系统钥匙串".to_string())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn load_secret(profile_id: &str) -> Result<Option<String>, String> {
    match entry(profile_id)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("无法从系统钥匙串读取凭据：{error}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn load_secret(_profile_id: &str) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn remove_secret(profile_id: &str) -> Result<(), String> {
    match entry(profile_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("无法从系统钥匙串删除凭据：{error}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn remove_secret(_profile_id: &str) -> Result<(), String> {
    Ok(())
}

pub fn resolve_secret(profile_id: &str, credential_ref: &str) -> Result<Option<String>, String> {
    let (scheme, reference) = credential_ref
        .split_once(':')
        .ok_or_else(|| "凭据引用格式无效".to_string())?;
    match scheme {
        "env" => match std::env::var(reference.trim()) {
            Ok(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_string())),
            Ok(_) => Ok(None),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("环境变量 {reference} 中的凭据不是有效 Unicode"))
            }
        },
        "keychain" | "credential" => load_secret(profile_id),
        _ => Err(format!("不支持的凭据引用 scheme：{scheme}")),
    }
}

pub fn has_secret(profile_id: &str) -> bool {
    load_secret(profile_id).ok().flatten().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn env_reference_resolves_without_storing_the_value_in_the_reference() {
        let name = format!("KITE_AGENT_TEST_CREDENTIAL_{}", std::process::id());
        let value = format!("random-credential-{}", std::process::id());
        unsafe { std::env::set_var(&name, &value) };
        let reference = format!("env:{name}");
        assert!(!reference.contains(&value));
        assert_eq!(
            resolve_secret("unused-profile", &reference)
                .expect("resolve env credential")
                .as_deref(),
            Some(value.as_str())
        );
        unsafe { std::env::remove_var(&name) };
    }

    #[test]
    #[ignore = "writes a temporary entry to the local system keychain"]
    fn keychain_round_trip_does_not_store_a_plaintext_reference() {
        let profile_id = format!(
            "agent-keychain-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let reference = save_secret(&profile_id, "fixture-secret").expect("save secret");
        assert!(reference.starts_with("keychain://"));
        assert!(!reference.contains("fixture-secret"));
        assert_eq!(
            load_secret(&profile_id).expect("load secret").as_deref(),
            Some("fixture-secret")
        );
        remove_secret(&profile_id).expect("remove secret");
        assert_eq!(load_secret(&profile_id).expect("load removed secret"), None);
    }
}
