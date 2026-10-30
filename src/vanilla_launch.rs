//! Vanilla launch planning, Java validation and child process supervision.
//! All I/O is blocking and must stay outside the GPUI event thread.
use crate::minecraft_data::{Artifact, Fetch, Installer};
use serde::Deserialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    io::{self},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
};

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

pub struct Identity {
    pub(crate) username: String,
    pub(crate) uuid: String,
    pub(crate) minecraft_token: String,
    pub(crate) owned_profile: bool,
}

pub struct LaunchPlan {
    java: PathBuf,
    work_dir: PathBuf,
    arguments: Vec<OsString>,
    natives: tempfile::TempDir,
    token: String,
}
/// Owns the process and its temporary native libraries until collection.
pub struct RunningGame {
    child: Child,
    _natives: tempfile::TempDir,
    token: String,
}
pub struct GameOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}
impl RunningGame {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.child.kill()
    }
    pub fn collect(self) -> io::Result<GameOutput> {
        let output = self.child.wait_with_output()?;
        let safe =
            |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).replace(&self.token, "[redacted]");
        Ok(GameOutput {
            status: output.status,
            stdout: safe(output.stdout),
            stderr: safe(output.stderr),
        })
    }
}
impl LaunchPlan {
    pub fn spawn(self) -> io::Result<RunningGame> {
        let child = Command::new(&self.java)
            .args(&self.arguments)
            .current_dir(&self.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(RunningGame {
            child,
            _natives: self.natives,
            token: self.token,
        })
    }
    /// Avoid exposing access tokens in debug output or user-facing log lines.
    pub fn summary(&self) -> String {
        format!(
            "Java: {}; directory: {}; arguments: [redacted]",
            self.java.display(),
            self.work_dir.display()
        )
    }
}

/// A per-instance preference. Managed Java is the default; manually selected runtimes are validated.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum JavaChoice {
    #[default]
    Managed,
    Custom(PathBuf),
}

impl JavaChoice {
    pub fn resolve<F: Fetch>(
        &self,
        installer: &Installer<F>,
        version: &str,
    ) -> io::Result<PathBuf> {
        let java = installer.version_java(version)?;
        let executable = match self {
            Self::Managed => {
                let platform = if cfg!(windows) {
                    "windows-x64"
                } else if cfg!(target_os = "macos") {
                    "mac-os"
                } else {
                    "linux"
                };
                installer.install_java(platform, &java.component, java.major_version)?
            }
            Self::Custom(path) => path.clone(),
        };
        check_java(&executable, java.major_version)?;
        Ok(executable)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Invalid Java setting location"))?;
        fs::create_dir_all(parent)?;
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(invalid("Refusing to replace Java settings link"));
        }
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut output, self).map_err(invalid)?;
        output.as_file().sync_all()?;
        output.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::Managed),
            Err(e) => Err(e),
        }
    }
}

/// Explicitly configured Java: Cinder-managed or user-supplied java/javaw path.
pub fn check_java(executable: &Path, required_major: u32) -> io::Result<JavaInspection> {
    if !fs::metadata(executable)?.is_file() {
        return Err(invalid("Java executable not a regular file"));
    }
    let result = Command::new(executable).arg("-version").output()?;
    if !result.status.success() {
        return Err(invalid("Java -version failed"));
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    inspect_java_version(&text, required_major)
}
#[derive(Debug, PartialEq, Eq)]
pub struct JavaInspection {
    pub major: u32,
    pub is_64_bit: bool,
}
pub fn inspect_java_version(output: &str, required: u32) -> io::Result<JavaInspection> {
    let first = output
        .lines()
        .find(|s| s.contains("version \"") || s.contains("openjdk "))
        .ok_or_else(|| invalid("Missing Java version"))?;
    let version = if let Some(pos) = first.find('"') {
        first[pos + 1..].split('"').next().unwrap_or("")
    } else {
        first.split_whitespace().nth(1).unwrap_or("")
    };
    let digits = version.split('.').next().unwrap_or("");
    let major = if digits == "1" {
        version
            .split('.')
            .nth(1)
            .unwrap_or("")
            .parse::<u32>()
            .map_err(invalid)?
    } else {
        digits.parse::<u32>().map_err(invalid)?
    };
    let is_64_bit =
        output.contains("64-Bit") || output.contains("64-bit") || output.contains("64 bit");
    if major != required || !is_64_bit {
        return Err(invalid(format!(
            "Java {} x64 required (detected Java {}, 64-bit={})",
            required, major, is_64_bit
        )));
    }
    Ok(JavaInspection { major, is_64_bit })
}

#[derive(Deserialize)]
struct Metadata {
    id: String,
    #[serde(rename = "mainClass")]
    main_class: String,
    arguments: Arguments,
    libraries: Vec<Library>,
    #[serde(rename = "assetIndex")]
    asset_index: AssetIndex,
    #[serde(rename = "javaVersion")]
    java_version: JavaRequirement,
    logging: Option<Logging>,
}
#[derive(Deserialize)]
struct Arguments {
    game: Vec<Value>,
    jvm: Vec<Value>,
}
#[derive(Deserialize)]
struct JavaRequirement {
    #[serde(rename = "majorVersion")]
    major: u32,
}
#[derive(Deserialize)]
struct AssetIndex {
    id: String,
}
#[derive(Deserialize)]
struct Library {
    downloads: Option<LibraryDownloads>,
    rules: Option<Vec<Rule>>,
    natives: Option<std::collections::HashMap<String, String>>,
    extract: Option<Extract>,
}
#[derive(Deserialize)]
struct LibraryDownloads {
    artifact: Option<LibraryFile>,
    classifiers: Option<std::collections::HashMap<String, LibraryFile>>,
}
#[derive(Deserialize)]
struct LibraryFile {
    path: String,
}
#[derive(Deserialize)]
struct Extract {
    exclude: Option<Vec<String>>,
}
#[derive(Deserialize)]
struct Logging {
    client: Option<ClientLogging>,
}
#[derive(Deserialize)]
struct ClientLogging {
    file: LoggingFile,
    argument: String,
}
#[derive(Deserialize)]
struct LoggingFile {
    id: String,
    #[serde(flatten)]
    artifact: Artifact,
}
#[derive(Deserialize)]
struct Rule {
    action: String,
    os: Option<OsRule>,
    features: Option<std::collections::HashMap<String, bool>>,
}
#[derive(Deserialize)]
struct OsRule {
    name: Option<String>,
    arch: Option<String>,
    version: Option<String>,
}
fn permitted(
    rules: Option<&[Rule]>,
    platform: &str,
    features: &std::collections::HashMap<String, bool>,
) -> bool {
    let Some(rules) = rules else { return true };
    let mut allowed = false;
    for rule in rules {
        let os_ok = rule.os.as_ref().is_none_or(|os| {
            os.name.as_deref().is_none_or(|name| name == platform)
                && os
                    .arch
                    .as_deref()
                    .is_none_or(|arch| arch == "x64" && cfg!(target_pointer_width = "64"))
                && os.version.as_deref().is_none() // Reject unhandled OS-version constraints, never silently allow.
        });
        let feature_ok = rule.features.as_ref().is_none_or(|conditions| {
            conditions
                .iter()
                .all(|(k, v)| features.get(k).copied().unwrap_or(false) == *v)
        });
        if os_ok && feature_ok {
            allowed = rule.action == "allow";
        }
    }
    allowed
}
fn append_arguments(
    items: &[Value],
    platform: &str,
    features: &std::collections::HashMap<String, bool>,
    replace: &std::collections::HashMap<&str, String>,
    out: &mut Vec<OsString>,
) -> io::Result<()> {
    for item in items {
        if let Some(s) = item.as_str() {
            out.push(expand(s, replace)?);
            continue;
        }
        let rules = item
            .get("rules")
            .map(|v| serde_json::from_value::<Vec<Rule>>(v.clone()).map_err(invalid))
            .transpose()?;
        if !permitted(rules.as_deref(), platform, features) {
            continue;
        }
        let value = item
            .get("value")
            .ok_or_else(|| invalid("Missing conditional argument"))?;
        if let Some(text) = value.as_str() {
            out.push(expand(text, replace)?);
        } else if let Some(array) = value.as_array() {
            for v in array {
                out.push(expand(
                    v.as_str()
                        .ok_or_else(|| invalid("Non-string JVM argument"))?,
                    replace,
                )?);
            }
        } else {
            return Err(invalid("Invalid launch argument"));
        }
    }
    Ok(())
}
fn expand(input: &str, map: &std::collections::HashMap<&str, String>) -> io::Result<OsString> {
    let mut s = String::new();
    let mut rest = input;
    while let Some(at) = rest.find("${") {
        s.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let close = tail
            .find('}')
            .ok_or_else(|| invalid("Unclosed argument placeholder"))?;
        let key = &tail[..close];
        s.push_str(
            map.get(key)
                .ok_or_else(|| invalid(format!("Unknown launch placeholder: {key}")))?,
        );
        rest = &tail[close + 1..];
    }
    s.push_str(rest);
    Ok(OsString::from(s))
}
fn safe_path(path: &str) -> io::Result<PathBuf> {
    use std::path::Component;
    let relative = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(invalid("Unsafe library path"));
    }
    Ok(relative.to_path_buf())
}
fn unpack_natives(jar: &Path, dest: &Path, excluded: &[String]) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    let source = fs::File::open(jar)?;
    let mut zip = zip::ZipArchive::new(source).map_err(invalid)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(invalid)?;
        if entry.is_dir() || excluded.iter().any(|e| entry.name().starts_with(e)) {
            continue;
        }
        let filename = entry
            .enclosed_name()
            .ok_or_else(|| invalid("Zip traversal"))?
            .to_path_buf();
        if !entry.name().ends_with(".dll")
            && !entry.name().ends_with(".so")
            && !entry.name().ends_with(".dylib")
        {
            continue;
        }
        let path = dest.join(filename);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut result = fs::File::create(path)?;
        io::copy(&mut entry, &mut result)?;
    }
    Ok(())
}

/// Requires completed P2 install, a validated owned account, and a Java runtime compatible with the version.
pub fn prepare<F: Fetch>(
    installer: &Installer<F>,
    version: &str,
    java: &Path,
    game_dir: &Path,
    identity: &Identity,
) -> io::Result<LaunchPlan> {
    if !identity.owned_profile
        || identity.username.is_empty()
        || identity.uuid.is_empty()
        || identity.minecraft_token.is_empty()
    {
        return Err(invalid("Verified Minecraft profile required"));
    }
    let info_path = installer
        .root
        .join("versions")
        .join(safe_path(version)?)
        .join("version.json");
    let meta: Metadata = serde_json::from_reader(fs::File::open(info_path)?).map_err(invalid)?;
    if meta.id != version {
        return Err(invalid("Version mismatch"));
    }
    check_java(java, meta.java_version.major)?;
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };
    let features = std::collections::HashMap::new();
    let mut libraries = Vec::new();
    let natives = tempfile::tempdir()?;
    let native_dir = natives.path().to_path_buf();
    for library in &meta.libraries {
        if !permitted(library.rules.as_deref(), platform, &features) {
            continue;
        }
        let Some(downloads) = &library.downloads else {
            continue;
        };
        if let Some(artifact) = &downloads.artifact {
            let path = installer
                .root
                .join("libraries")
                .join(safe_path(&artifact.path)?);
            if !path.is_file() {
                return Err(invalid("Required library missing; run P2 installer"));
            }
            libraries.push(path);
        }
        if let Some(classifier) = library.natives.as_ref().and_then(|v| v.get(platform)) {
            let name = classifier.replace(
                "${arch}",
                if cfg!(target_pointer_width = "64") {
                    "64"
                } else {
                    "32"
                },
            );
            let file = downloads
                .classifiers
                .as_ref()
                .and_then(|v| v.get(&name))
                .ok_or_else(|| invalid("Missing native library classifier"))?;
            let jar = installer
                .root
                .join("libraries")
                .join(safe_path(&file.path)?);
            unpack_natives(
                &jar,
                &native_dir,
                library
                    .extract
                    .as_ref()
                    .and_then(|e| e.exclude.as_deref())
                    .unwrap_or(&["META-INF/".to_owned()]),
            )?;
        }
    }
    let client = installer
        .root
        .join("versions")
        .join(safe_path(version)?)
        .join("client.jar");
    if !client.is_file() {
        return Err(invalid("Minecraft JAR missing"));
    }
    libraries.push(client);
    fs::create_dir_all(game_dir)?;
    let separator = if cfg!(windows) { ";" } else { ":" };
    let classpath = libraries
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(separator);
    let mut map = std::collections::HashMap::new();
    map.insert("natives_directory", native_dir.display().to_string());
    map.insert("launcher_name", "Cinder Launcher".into());
    map.insert("launcher_version", env!("CARGO_PKG_VERSION").into());
    map.insert("classpath", classpath);
    map.insert("classpath_separator", separator.into());
    map.insert(
        "library_directory",
        installer.root.join("libraries").display().to_string(),
    );
    map.insert("auth_player_name", identity.username.clone());
    map.insert("version_name", version.to_owned());
    map.insert("game_directory", game_dir.display().to_string());
    map.insert(
        "assets_root",
        installer.root.join("assets").display().to_string(),
    );
    map.insert("assets_index_name", meta.asset_index.id);
    map.insert("auth_uuid", identity.uuid.clone());
    map.insert("auth_access_token", identity.minecraft_token.clone());
    map.insert("clientid", String::new());
    map.insert("auth_xuid", String::new());
    map.insert("user_type", "msa".into());
    map.insert("version_type", "release".into());
    map.insert("user_properties", "{}".into());
    map.insert("resolution_width", "854".into());
    map.insert("resolution_height", "480".into());
    let mut args = Vec::new();
    append_arguments(&meta.arguments.jvm, platform, &features, &map, &mut args)?;
    if let Some(logging) = meta.logging.and_then(|l| l.client) {
        let dest = installer
            .root
            .join("logging")
            .join(safe_path(&logging.file.id)?);
        installer.download(&logging.file.artifact, &dest)?;
        map.insert("path", dest.display().to_string());
        args.push(expand(&logging.argument, &map)?);
    }
    args.push(meta.main_class.into());
    append_arguments(&meta.arguments.game, platform, &features, &map, &mut args)?;
    Ok(LaunchPlan {
        java: java.to_path_buf(),
        work_dir: game_dir.to_path_buf(),
        arguments: args,
        natives,
        token: identity.minecraft_token.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn java_choice_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("java-choice.json");
        assert_eq!(JavaChoice::load(&path).unwrap(), JavaChoice::Managed);
        let custom = JavaChoice::Custom(PathBuf::from("C:/Apps/JDK/bin/javaw.exe"));
        custom.save(&path).unwrap();
        assert_eq!(JavaChoice::load(&path).unwrap(), custom);
    }
    #[test]
    fn java_parser_detects_version_and_arch() {
        let expected = "openjdk version \"21.0.7\" 2025-04-15 LTS\nOpenJDK 64-Bit Server VM";
        assert_eq!(
            inspect_java_version(expected, 21).unwrap(),
            JavaInspection {
                major: 21,
                is_64_bit: true
            }
        );
        assert!(inspect_java_version(expected, 17).is_err());
        assert!(inspect_java_version("openjdk version \"21.0.7\"\nOpenJDK Client VM", 21).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn offline_launch_plan_redacts_token_and_preserves_native_lifetime() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let java = t.path().join("fake-java");
        std::fs::write(&java,"#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'openjdk version \"21.0.7\"' >&2; echo 'OpenJDK 64-Bit Server VM' >&2; exit 0; fi\necho -n secret-test-token\n").unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = t.path().join("assets");
        std::fs::create_dir_all(root.join("versions/1.21.1")).unwrap();
        let metadata = serde_json::json!({
            "id":"1.21.1", "mainClass":"net.minecraft.client.main.Main",
            "javaVersion":{"majorVersion":21},"assetIndex":{"id":"17"},
            "arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}","--accessToken","${auth_access_token}"]},
            "libraries":[]
        });
        std::fs::write(
            root.join("versions/1.21.1/version.json"),
            metadata.to_string(),
        )
        .unwrap();
        std::fs::write(root.join("versions/1.21.1/client.jar"), "mock").unwrap();
        struct NoNetwork;
        impl Fetch for NoNetwork {
            fn open(&self, _: &str) -> io::Result<Box<dyn std::io::Read>> {
                Err(invalid("Unexpected network access"))
            }
        }
        let installer = Installer {
            fetcher: NoNetwork,
            root,
        };
        let identity = Identity {
            username: "Player".into(),
            uuid: "0123456789abcdef0123456789abcdef".into(),
            minecraft_token: "secret-test-token".into(),
            owned_profile: true,
        };
        let plan = prepare(
            &installer,
            "1.21.1",
            &java,
            &t.path().join("world"),
            &identity,
        )
        .unwrap();
        assert!(!plan.summary().contains("secret-test-token"));
        let process = plan.spawn().unwrap();
        let result = process.collect().unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, "[redacted]");
    }
    #[test]
    fn rules_and_arguments() {
        let rules = serde_json::from_value::<Vec<Rule>>(
            serde_json::json!([{"action":"allow","os":{"name":"windows"}}]),
        )
        .unwrap();
        assert!(permitted(Some(&rules), "windows", &Default::default()));
        assert!(!permitted(Some(&rules), "linux", &Default::default()));
        let mut out = Vec::new();
        let features = Default::default();
        let map = std::collections::HashMap::from([("token", "secret".to_owned())]);
        append_arguments(
            &[serde_json::json!("--token"), serde_json::json!("${token}")],
            "windows",
            &features,
            &map,
            &mut out,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![OsString::from("--token"), OsString::from("secret")]
        );
        assert!(expand("${missing}", &map).is_err());
        assert!(safe_path("../world").is_err());
    }
}
