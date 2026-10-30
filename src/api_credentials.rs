//! External service identifiers and credentials. Secrets are never written to ordinary settings.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    CurseForge,
    Modrinth,
    MicrosoftRefresh,
}
impl SecretKind {
    fn target(self) -> &'static str {
        match self {
            Self::CurseForge => "CurseForge",
            Self::Modrinth => "Modrinth",
            Self::MicrosoftRefresh => "MicrosoftRefresh",
        }
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn config_path() -> io::Result<PathBuf> {
    directories::ProjectDirs::from("", "Cinder", "Cinder Launcher")
        .map(|dirs| dirs.config_dir().join("services.json"))
        .ok_or_else(|| invalid("Application configuration folder not available"))
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct PublicSettings {
    pub microsoft_client_id: Option<String>,
}
impl PublicSettings {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid service settings"))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    pub fn set_microsoft_id(&mut self, id: &str) -> io::Result<()> {
        let candidate = id.trim();
        if !candidate.is_empty() && uuid::Uuid::parse_str(candidate).is_err() {
            return Err(invalid("Invalid Microsoft client ID"));
        }
        self.microsoft_client_id = if candidate.is_empty() {
            None
        } else {
            Some(candidate.into())
        };
        Ok(())
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Invalid service settings path"))?;
        fs::create_dir_all(parent)?;
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(invalid("Service settings cannot be a symlink"));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut temp, self).map_err(io::Error::other)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Per-user Windows Credential Manager; never shares credentials across machines.
#[derive(Clone)]
pub struct SecretStore {
    namespace: String,
}
impl Default for SecretStore {
    fn default() -> Self {
        Self {
            namespace: "CinderLauncher".into(),
        }
    }
}
impl SecretStore {
    fn target(&self, kind: SecretKind) -> String {
        format!("{}:{}", self.namespace, kind.target())
    }
    pub fn set(&self, kind: SecretKind, value: &str) -> io::Result<()> {
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(invalid(
                "Secret cannot be empty or contain control characters",
            ));
        }
        secure::put(&self.target(kind), value)
    }
    pub fn get(&self, kind: SecretKind) -> io::Result<Option<String>> {
        secure::get(&self.target(kind))
    }
    pub fn is_configured(&self, kind: SecretKind) -> io::Result<bool> {
        Ok(self.get(kind)?.is_some())
    }
    pub fn delete(&self, kind: SecretKind) -> io::Result<()> {
        secure::delete(&self.target(kind))
    }
}

#[cfg(windows)]
mod secure {
    use super::*;
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::ERROR_NOT_FOUND,
        Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
            CredReadW, CredWriteW,
        },
    };
    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }
    pub fn put(target: &str, value: &str) -> io::Result<()> {
        let mut name = wide(target);
        let mut bytes = value.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            CredentialBlobSize: bytes
                .len()
                .try_into()
                .map_err(|_| invalid("Credential too long"))?,
            CredentialBlob: bytes.as_mut_ptr(),
            ..Default::default()
        };
        let result = unsafe { CredWriteW(&credential, 0) };
        bytes.fill(0);
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub fn get(target: &str) -> io::Result<Option<String>> {
        let name = wide(target);
        let mut ptr = ptr::null_mut();
        let result = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut ptr) };
        if result == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
                return Ok(None);
            }
            return Err(e);
        }
        struct Release(*mut CREDENTIALW);
        impl Drop for Release {
            fn drop(&mut self) {
                unsafe { CredFree(self.0.cast()) }
            }
        }
        let _release = Release(ptr);
        let value = unsafe {
            let entry = &*ptr;
            if entry.CredentialBlobSize == 0 {
                return Ok(Some(String::new()));
            }
            let raw =
                std::slice::from_raw_parts(entry.CredentialBlob, entry.CredentialBlobSize as usize);
            std::str::from_utf8(raw)
                .map_err(|_| invalid("Saved credential has invalid text"))?
                .to_owned()
        };
        Ok(Some(value))
    }
    pub fn delete(target: &str) -> io::Result<()> {
        let name = wide(target);
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                return Err(e);
            }
        }
        Ok(())
    }
}
#[cfg(not(windows))]
mod secure {
    use super::*;
    fn unavailable() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Secure credential storage is supported on Windows only",
        )
    }
    pub fn put(_: &str, _: &str) -> io::Result<()> {
        Err(unavailable())
    }
    pub fn get(_: &str) -> io::Result<Option<String>> {
        Err(unavailable())
    }
    pub fn delete(_: &str) -> io::Result<()> {
        // There is no persisted secret on unsupported platforms to remove.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_id_roundtrip_without_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("config/services.json");
        let mut settings = PublicSettings::load(&file).unwrap();
        assert!(settings.microsoft_client_id.is_none());
        settings
            .set_microsoft_id("8f62d03f-21cd-48db-ab9f-367da2e9f4a5")
            .unwrap();
        assert!(settings.set_microsoft_id("not-guid").is_err());
        settings.save(&file).unwrap();
        let reloaded = PublicSettings::load(&file).unwrap();
        assert_eq!(reloaded.microsoft_client_id, settings.microsoft_client_id);
        assert!(
            !String::from_utf8(fs::read(file).unwrap())
                .unwrap()
                .contains("CurseForge")
        );
    }
    #[test]
    fn reject_empty_secret() {
        assert!(
            SecretStore::default()
                .set(SecretKind::CurseForge, "\n")
                .is_err()
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_credential_roundtrip_and_delete() {
        let store = SecretStore {
            namespace: format!("CinderLauncher-Test-{}", uuid::Uuid::new_v4()),
        };
        assert!(!store.is_configured(SecretKind::CurseForge).unwrap());
        store
            .set(SecretKind::CurseForge, "isolated-test-key")
            .unwrap();
        assert_eq!(
            store.get(SecretKind::CurseForge).unwrap().as_deref(),
            Some("isolated-test-key")
        );
        assert!(!store.is_configured(SecretKind::Modrinth).unwrap());
        store.delete(SecretKind::CurseForge).unwrap();
        assert!(!store.is_configured(SecretKind::CurseForge).unwrap());
    }
}
