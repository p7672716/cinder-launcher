//! Blocking P3 orchestration. Call only on a background executor.
use crate::{
    api_credentials::{PublicSettings, SecretKind, SecretStore},
    instance::Loader,
    instance_store::InstanceStore,
    microsoft_auth::{self, OwnedSession},
    minecraft_data::{HttpFetch, Installer},
    vanilla_launch::{self, JavaChoice, RunningGame},
};
use std::{io, path::Path};

fn no_account() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Register a Microsoft client ID in settings first",
    )
}
fn secret_store_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "No saved Microsoft sign-in. Use the sign-in button",
    )
}
fn client_id(path: &Path) -> io::Result<String> {
    PublicSettings::load(path)?
        .microsoft_client_id
        .ok_or_else(no_account)
}

pub fn sign_in(path: &Path, store: &SecretStore) -> io::Result<OwnedSession> {
    let id = client_id(path)?;
    let session = crate::oauth_loopback::authenticate(&id)?;
    save_refresh(store, &session)?;
    Ok(session)
}

pub fn restore_session(path: &Path, store: &SecretStore) -> io::Result<OwnedSession> {
    let id = client_id(path)?;
    let previous = store
        .get(SecretKind::MicrosoftRefresh)?
        .ok_or_else(secret_store_error)?;
    let session = microsoft_auth::resume(&microsoft_auth::AuthHttp::new()?, &id, &previous)?;
    save_refresh(store, &session)?;
    Ok(session)
}
fn save_refresh(store: &SecretStore, session: &OwnedSession) -> io::Result<()> {
    let refresh = session.refresh_secret().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Microsoft did not issue an offline refresh token",
        )
    })?;
    store.set(SecretKind::MicrosoftRefresh, refresh)
}
pub fn logout(store: &SecretStore) -> io::Result<()> {
    store.delete(SecretKind::MicrosoftRefresh)
}

pub fn java_selection_path(store: &InstanceStore, id: uuid::Uuid) -> std::path::PathBuf {
    // Stored next to the instance profile, not in user-owned Minecraft game/ data.
    store
        .game_dir(id)
        .parent()
        .expect("instance directory")
        .join("java.json")
}
fn installer() -> io::Result<Installer<HttpFetch>> {
    Ok(Installer {
        fetcher: HttpFetch::new()?,
        root: Installer::<HttpFetch>::default_root()?,
    })
}
fn vanilla_version(store: &InstanceStore, id: uuid::Uuid) -> io::Result<String> {
    let list = store.list()?;
    let instance = list
        .iter()
        .find(|i| i.id() == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Instance no longer exists"))?;
    if !matches!(instance.target().loader, Loader::Vanilla) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Only Vanilla launch is implemented",
        ));
    }
    Ok(instance.target().minecraft_version.to_owned())
}
fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub fn install(store: &InstanceStore, id: uuid::Uuid) -> io::Result<()> {
    let version = vanilla_version(store, id)?;
    let installer = installer()?;
    installer.install_version(&version, platform())?;
    let choice = JavaChoice::load(&java_selection_path(store, id))?;
    choice.resolve(&installer, &version)?;
    Ok(())
}

pub fn launch(
    store: &InstanceStore,
    id: uuid::Uuid,
    identity: &vanilla_launch::Identity,
) -> io::Result<RunningGame> {
    let version = vanilla_version(store, id)?;
    let installer = installer()?;
    // Re-verify missing/corrupt downloads before the game is ever started.
    installer.install_version(&version, platform())?;
    let java = JavaChoice::load(&java_selection_path(store, id))?.resolve(&installer, &version)?;
    let dir = store.game_dir(id);
    let plan = vanilla_launch::prepare(&installer, &version, &java, &dir, identity)?;
    plan.spawn()
}

pub fn save_java(store: &InstanceStore, id: uuid::Uuid, choice: &JavaChoice) -> io::Result<()> {
    let version = vanilla_version(store, id)?;
    if let JavaChoice::Custom(executable) = choice {
        let java = installer()?.version_java(&version)?;
        vanilla_launch::check_java(executable, java.major_version)?;
    }
    let path = java_selection_path(store, id);
    // `store.list()` above verifies the profile and its containing directory.
    choice.save(&path)
}

pub fn current_java(store: &InstanceStore, id: uuid::Uuid) -> io::Result<JavaChoice> {
    vanilla_version(store, id)?;
    JavaChoice::load(&java_selection_path(store, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn java_setting_is_per_instance_and_never_touches_world() {
        let t = tempfile::tempdir().unwrap();
        let store = InstanceStore::new(t.path().join("instances"));
        let target = crate::instance::GameTarget {
            minecraft_version: "1.21.1".into(),
            loader: Loader::Vanilla,
        };
        let a = store.create("first", target.clone()).unwrap();
        let b = store.create("second", target).unwrap();
        let world = store.game_dir(a.id()).join("saves/user-world.txt");
        std::fs::create_dir_all(world.parent().unwrap()).unwrap();
        std::fs::write(&world, "important save").unwrap();
        assert!(matches!(
            current_java(&store, a.id()).unwrap(),
            JavaChoice::Managed
        ));
        save_java(&store, a.id(), &JavaChoice::Managed).unwrap();
        assert!(java_selection_path(&store, a.id()).exists());
        assert!(!java_selection_path(&store, b.id()).exists());
        assert_eq!(std::fs::read_to_string(world).unwrap(), "important save");
    }
    #[test]
    fn absent_login_does_not_fall_back_to_another_client() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(client_id(&tmp.path().join("missing.json")).is_err());
        assert!(logout(&SecretStore::default()).is_ok());
    }
}
