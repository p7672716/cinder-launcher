//! GUI-independent, in-memory definitions. No Minecraft file I/O occurs here.
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameTarget {
    pub minecraft_version: String,
    pub loader: Loader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loader {
    Vanilla,
    NeoForge { version: Option<String> },
}

impl GameTarget {
    pub fn is_selected(&self) -> bool {
        !self.minecraft_version.trim().is_empty()
            && match &self.loader {
                Loader::Vanilla => true,
                Loader::NeoForge { version } => {
                    version.as_ref().is_some_and(|v| !v.trim().is_empty())
                }
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    NotInstalled,
    Installing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchState {
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileState {
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceError {
    InvalidName,
    DuplicateId,
    TargetIncomplete,
    InvalidTransition,
    VerificationFailed,
}

pub fn validate_name(name: &str) -> Result<(), InstanceError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(InstanceError::InvalidName);
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Instance {
    id: Uuid,
    name: String,
    created_at: SystemTime,
    target: GameTarget,
    profile_state: ProfileState,
    install_state: InstallState,
    launch_state: LaunchState,
}

impl Instance {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn target(&self) -> &GameTarget {
        &self.target
    }
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }
    pub fn profile_state(&self) -> &ProfileState {
        &self.profile_state
    }
    pub fn install_state(&self) -> &InstallState {
        &self.install_state
    }
    pub fn launch_state(&self) -> &LaunchState {
        &self.launch_state
    }

    pub fn new(
        id: Uuid,
        name: &str,
        target: GameTarget,
        created_at: SystemTime,
    ) -> Result<Self, InstanceError> {
        validate_name(name)?;
        Ok(Self {
            id,
            name: name.to_owned(),
            created_at,
            target,
            profile_state: ProfileState::Active,
            install_state: InstallState::NotInstalled,
            launch_state: LaunchState::Stopped,
        })
    }

    pub fn rename(&mut self, name: &str) -> Result<(), InstanceError> {
        validate_name(name)?;
        self.name = name.to_owned();
        Ok(())
    }

    pub fn set_target(&mut self, target: GameTarget) {
        if self.target != target {
            self.target = target;
            self.install_state = InstallState::NotInstalled;
        }
    }

    pub fn start_install(&mut self) -> Result<(), InstanceError> {
        if !self.target.is_selected() {
            return Err(InstanceError::TargetIncomplete);
        }
        if self.install_state == InstallState::Installing {
            return Err(InstanceError::InvalidTransition);
        }
        self.install_state = InstallState::Installing;
        Ok(())
    }

    pub fn finish_install(
        &mut self,
        verified_target: &GameTarget,
        artifacts_verified: bool,
    ) -> Result<(), InstanceError> {
        if self.install_state != InstallState::Installing {
            return Err(InstanceError::InvalidTransition);
        }
        if !artifacts_verified || self.target != *verified_target || !self.target.is_selected() {
            return Err(InstanceError::VerificationFailed);
        }
        self.install_state = InstallState::Ready;
        Ok(())
    }

    pub fn fail_install(&mut self) -> Result<(), InstanceError> {
        if self.install_state != InstallState::Installing {
            return Err(InstanceError::InvalidTransition);
        }
        self.install_state = InstallState::Failed;
        Ok(())
    }

    pub fn start_launch(&mut self) -> Result<(), InstanceError> {
        if self.launch_state != LaunchState::Stopped || self.install_state != InstallState::Ready {
            return Err(InstanceError::InvalidTransition);
        }
        self.launch_state = LaunchState::Starting;
        Ok(())
    }

    pub fn process_started(&mut self) -> Result<(), InstanceError> {
        if self.launch_state != LaunchState::Starting {
            return Err(InstanceError::InvalidTransition);
        }
        self.launch_state = LaunchState::Running;
        Ok(())
    }

    pub fn process_exited(&mut self) -> Result<(), InstanceError> {
        if self.launch_state != LaunchState::Running && self.launch_state != LaunchState::Starting {
            return Err(InstanceError::InvalidTransition);
        }
        self.launch_state = LaunchState::Stopped;
        Ok(())
    }
}

#[derive(Default)]
pub struct InstanceList(Vec<Instance>);

impl InstanceList {
    pub fn add(&mut self, instance: Instance) -> Result<(), InstanceError> {
        if self.0.iter().any(|item| item.id == instance.id) {
            return Err(InstanceError::DuplicateId);
        }
        self.0.push(instance);
        Ok(())
    }

    pub fn items(&self) -> &[Instance] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModSource {
    Manual,
    Modrinth { project_id: String, file_id: String },
    CurseForge { project_id: String, file_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    Unknown,
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModEntry {
    pub source: ModSource,
    pub compatibility: Compatibility,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> GameTarget {
        GameTarget {
            minecraft_version: "1.21.1".into(),
            loader: Loader::NeoForge { version: None },
        }
    }
    fn instance() -> Instance {
        Instance::new(Uuid::new_v4(), "Pack A", target(), SystemTime::UNIX_EPOCH).unwrap()
    }

    #[test]
    fn p0_01_initial_definition_is_not_installed() {
        let item = instance();
        assert_eq!(item.name, "Pack A");
        assert_eq!(item.target, target());
        assert_eq!(item.install_state, InstallState::NotInstalled);
    }
    #[test]
    fn p0_02_reject_invalid_names_without_mutation() {
        let mut item = instance();
        for name in ["", "  ", "Hello\nWorld", "x\u{0}y"] {
            assert_eq!(item.rename(name), Err(InstanceError::InvalidName));
            assert!(Instance::new(Uuid::new_v4(), name, target(), SystemTime::UNIX_EPOCH).is_err());
            assert_eq!(item.name, "Pack A");
        }
    }
    #[test]
    fn p0_04_distinct_ids_can_have_same_name() {
        let a = instance();
        let b = instance();
        let mut list = InstanceList::default();
        list.add(a.clone()).unwrap();
        list.add(b.clone()).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(list.items().len(), 2);
    }
    #[test]
    fn p0_05_rename_preserves_identity_target_and_creation_time() {
        let mut item = instance();
        let id = item.id;
        let created = item.created_at;
        let target = item.target.clone();
        item.rename("My Pack").unwrap();
        assert_eq!(item.name, "My Pack");
        assert_eq!(
            (item.id, item.created_at, item.target),
            (id, created, target)
        );
    }
    #[test]
    fn p0_06_duplicate_id_rejected_without_overwrite() {
        let a = instance();
        let mut b = instance();
        b.id = a.id;
        b.name = "Pack B".into();
        let mut list = InstanceList::default();
        list.add(a).unwrap();
        assert_eq!(list.add(b), Err(InstanceError::DuplicateId));
        assert_eq!(list.items()[0].name, "Pack A");
    }
    #[test]
    fn p0_07_loader_build_not_assumed() {
        let mut item = instance();
        assert!(!item.target.is_selected());
        assert_eq!(item.start_install(), Err(InstanceError::TargetIncomplete));
        assert_eq!(item.install_state, InstallState::NotInstalled);
        item.target.loader = Loader::NeoForge {
            version: Some("21.1.0".into()),
        };
        assert!(item.target.is_selected());
    }
    #[test]
    fn p0_08_changed_target_invalidates_ready() {
        let mut item = instance();
        item.target.loader = Loader::NeoForge {
            version: Some("21.1.0".into()),
        };
        item.start_install().unwrap();
        item.finish_install(&item.target.clone(), true).unwrap();
        item.set_target(GameTarget {
            minecraft_version: "1.21.2".into(),
            ..item.target.clone()
        });
        assert_eq!(item.install_state, InstallState::NotInstalled);
    }
    #[test]
    fn p0_09_partial_install_cannot_be_ready() {
        let mut item = instance();
        item.target.loader = Loader::Vanilla;
        item.start_install().unwrap();
        assert_eq!(
            item.finish_install(&item.target.clone(), false),
            Err(InstanceError::VerificationFailed)
        );
        assert_eq!(item.install_state, InstallState::Installing);
    }
    #[test]
    fn p0_10_only_verified_matching_target_is_ready() {
        let mut item = instance();
        item.target.loader = Loader::Vanilla;
        item.start_install().unwrap();
        assert_eq!(
            item.finish_install(&target(), true),
            Err(InstanceError::VerificationFailed)
        );
        item.finish_install(&item.target.clone(), true).unwrap();
        assert_eq!(item.install_state, InstallState::Ready);
        item.set_target(target());
        item.target.loader = Loader::Vanilla;
        item.start_install().unwrap();
        item.fail_install().unwrap();
        assert_eq!(item.install_state, InstallState::Failed);
    }
    #[test]
    fn p0_11_launch_states_follow_process_confirmation() {
        let mut item = instance();
        item.target.loader = Loader::Vanilla;
        item.start_install().unwrap();
        item.finish_install(&item.target.clone(), true).unwrap();
        item.start_launch().unwrap();
        assert_eq!(item.launch_state, LaunchState::Starting);
        item.process_started().unwrap();
        assert_eq!(item.launch_state, LaunchState::Running);
        item.process_exited().unwrap();
        assert_eq!(item.launch_state, LaunchState::Stopped);
    }
    #[test]
    fn p0_12_install_and_launch_state_are_independent() {
        let mut item = instance();
        item.target.loader = Loader::Vanilla;
        item.start_install().unwrap();
        assert_eq!(item.launch_state, LaunchState::Stopped);
        assert_eq!(item.profile_state, ProfileState::Active);
    }
    #[test]
    fn p0_13_no_credentials_in_instance_data() {
        let item = instance();
        let snapshot = format!("{item:?}");
        assert!(!snapshot.contains("access_token"));
        assert!(!snapshot.contains("refresh_token"));
        assert!(!snapshot.contains("secret"));
    }
    #[test]
    fn p0_14_manual_mod_does_not_require_provider_ids() {
        let entry = ModEntry {
            source: ModSource::Manual,
            compatibility: Compatibility::Unknown,
        };
        assert_eq!(entry.compatibility, Compatibility::Unknown);
    }
}
