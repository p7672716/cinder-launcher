//! Minecraft 1.21.1 files and Mojang-managed Java runtimes.
//! Network operations are blocking: call from a worker, never the GPUI UI thread.
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

pub const VERSIONS_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const RUNTIMES_URL: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
pub const ASSETS_HOST: &str = "https://resources.download.minecraft.net";

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

pub trait Fetch {
    fn open(&self, url: &str) -> io::Result<Box<dyn Read>>;
}

pub struct HttpFetch {
    client: reqwest::blocking::Client,
}
impl HttpFetch {
    pub fn new() -> io::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(io::Error::other)?;
        Ok(Self { client })
    }
}
impl Fetch for HttpFetch {
    fn open(&self, url: &str) -> io::Result<Box<dyn Read>> {
        let uri = reqwest::Url::parse(url).map_err(invalid)?;
        if uri.scheme() != "https"
            || !matches!(
                uri.host_str(),
                Some(
                    "piston-meta.mojang.com"
                        | "piston-data.mojang.com"
                        | "libraries.minecraft.net"
                        | "resources.download.minecraft.net"
                        | "launchermeta.mojang.com"
                )
            )
        {
            return Err(invalid("Not an official Minecraft download URL"));
        }
        let response = self
            .client
            .get(uri)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(io::Error::other)?;
        Ok(Box::new(response))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
fn verify(path: &Path, artifact: &Artifact) -> io::Result<bool> {
    if !valid_sha(&artifact.sha1) {
        return Err(invalid("Invalid expected SHA-1"));
    }
    if !path.exists() {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != artifact.size
    {
        return Ok(false);
    }
    let mut input = fs::File::open(path)?;
    let mut hash = Sha1::new();
    io::copy(&mut input, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&artifact.sha1))
}
fn ensure_dir(path: &Path) -> io::Result<()> {
    let mut chain = Vec::new();
    for p in path.ancestors() {
        chain.push(p);
    }
    for dir in chain.into_iter().rev() {
        if dir.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() =>
            {
                #[cfg(windows)]
                if std::os::windows::fs::MetadataExt::file_attributes(&meta) & 0x400 != 0 {
                    return Err(invalid("Directory is a reparse point"));
                }
            }
            Ok(_) => return Err(invalid("Directory is a link or not a directory")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(dir)?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn relative(s: &str) -> io::Result<PathBuf> {
    if s.is_empty() || s.contains('\\') || s.contains(':') {
        return Err(invalid("Invalid relative path"));
    }
    let p = Path::new(s);
    if !p.components().all(|x| matches!(x, Component::Normal(_))) {
        return Err(invalid("Unsafe relative path"));
    }
    Ok(p.to_owned())
}

#[derive(Clone)]
pub struct Installer<F: Fetch> {
    pub fetcher: F,
    pub root: PathBuf,
}
impl<F: Fetch> Installer<F> {
    pub fn default_root() -> io::Result<PathBuf> {
        directories::ProjectDirs::from("", "Cinder", "Cinder Launcher")
            .map(|d| d.data_dir().join("downloads"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No application data folder"))
    }
    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        artifact: &Artifact,
        path: &Path,
    ) -> io::Result<T> {
        self.download(artifact, path)?;
        serde_json::from_reader(fs::File::open(path)?).map_err(invalid)
    }
    fn version_json<T: serde::de::DeserializeOwned>(
        &self,
        entry: &VersionEntry,
        path: &Path,
    ) -> io::Result<T> {
        if !valid_sha(&entry.sha1) {
            return Err(invalid("Invalid version metadata checksum"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Missing version path"))?;
        ensure_dir(parent)?;
        let bytes = if path.exists() {
            fs::read(path)?
        } else {
            Vec::new()
        };
        let bytes = if !bytes.is_empty()
            && format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(&entry.sha1)
        {
            bytes
        } else {
            let mut source = self.fetcher.open(&entry.url)?;
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes)?;
            if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(&entry.sha1) {
                return Err(invalid("Version JSON checksum mismatch"));
            }
            let mut staged = tempfile::NamedTempFile::new_in(parent)?;
            staged.write_all(&bytes)?;
            staged.as_file().sync_all()?;
            staged.persist(path).map_err(|e| e.error)?;
            bytes
        };
        serde_json::from_slice(&bytes).map_err(invalid)
    }
    pub fn download(&self, artifact: &Artifact, path: &Path) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| invalid("Missing parent"))?;
        ensure_dir(parent)?;
        if verify(path, artifact)? {
            return Ok(());
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let mut source = self.fetcher.open(&artifact.url)?;
        let mut hash = Sha1::new();
        let mut count = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            count = count
                .checked_add(n as u64)
                .ok_or_else(|| invalid("Transfer size overflow"))?;
            if count > artifact.size {
                return Err(invalid("Transfer exceeds declared size"));
            }
            temp.write_all(&buf[..n])?;
            hash.update(&buf[..n]);
        }
        if count != artifact.size
            || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&artifact.sha1)
        {
            return Err(invalid("Download checksum or length mismatch"));
        }
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    pub fn version_java(&self, version: &str) -> io::Result<JavaVersion> {
        Ok(self.read_version(version)?.java_version)
    }
    fn read_version(&self, version: &str) -> io::Result<VersionDetail> {
        let index: VersionIndex = self.fetch_json(VERSIONS_URL)?;
        let entry = index
            .versions
            .iter()
            .find(|v| v.id == version)
            .ok_or_else(|| invalid("Version not present in official manifest"))?;
        let version_root = self.root.join("versions").join(relative(version)?);
        let metadata: VersionDetail =
            self.version_json(entry, &version_root.join("version.json"))?;
        if metadata.id != version {
            return Err(invalid("Version metadata ID mismatch"));
        }
        Ok(metadata)
    }
    pub fn install_version(&self, version: &str, platform: &str) -> io::Result<VersionResult> {
        if !matches!(platform, "windows" | "linux" | "osx") {
            return Err(invalid("Unsupported platform"));
        }
        let version_root = self.root.join("versions").join(relative(version)?);
        let metadata = self.read_version(version)?;
        self.download(&metadata.downloads.client, &version_root.join("client.jar"))?;
        let assets: AssetIndex = self.get_json(
            &metadata.asset_index,
            &self
                .root
                .join("assets/indexes")
                .join(format!("{}.json", relative(&metadata.assets)?.display())),
        )?;
        let mut assets_count = 0;
        for item in assets.objects.values() {
            if !valid_sha(&item.hash) {
                return Err(invalid("Invalid asset hash"));
            }
            let dest = self
                .root
                .join("assets/objects")
                .join(&item.hash[..2])
                .join(&item.hash);
            self.download(
                &Artifact {
                    url: format!("{}/{}/{}", ASSETS_HOST, &item.hash[..2], &item.hash),
                    sha1: item.hash.clone(),
                    size: item.size,
                },
                &dest,
            )?;
            assets_count += 1;
        }
        let mut libraries_count = 0;
        for lib in &metadata.libraries {
            if !lib.allowed(platform) {
                continue;
            }
            let Some(downloads) = &lib.downloads else {
                continue;
            };
            if let Some(a) = &downloads.artifact {
                self.download(
                    &a.as_artifact(),
                    &self.root.join("libraries").join(relative(&a.path)?),
                )?;
                libraries_count += 1;
            }
            if let Some(native) = lib.natives.as_ref().and_then(|n| n.get(platform)) {
                let arch = native.replace(
                    "${arch}",
                    if cfg!(target_pointer_width = "64") {
                        "64"
                    } else {
                        "32"
                    },
                );
                if let Some(item) = downloads.classifiers.as_ref().and_then(|c| c.get(&arch)) {
                    self.download(
                        &item.as_artifact(),
                        &self.root.join("libraries").join(relative(&item.path)?),
                    )?;
                    libraries_count += 1;
                }
            }
        }
        Ok(VersionResult {
            id: metadata.id,
            java: metadata.java_version,
            assets: assets_count,
            libraries: libraries_count,
        })
    }
    fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> io::Result<T> {
        serde_json::from_reader(self.fetcher.open(url)?).map_err(invalid)
    }
    pub fn install_java(&self, platform: &str, component: &str, major: u32) -> io::Result<PathBuf> {
        if !matches!(
            platform,
            "windows-x64" | "linux" | "mac-os" | "mac-os-arm64"
        ) {
            return Err(invalid("Unsupported runtime platform"));
        }
        let index: HashMap<String, HashMap<String, Vec<RuntimeEntry>>> =
            self.fetch_json(RUNTIMES_URL)?;
        let list = index
            .get(platform)
            .and_then(|platform| platform.get(component))
            .ok_or_else(|| invalid("Java runtime component unavailable"))?;
        let chosen = list
            .first()
            .ok_or_else(|| invalid("No Java runtime release"))?;
        let path = self
            .root
            .join("java")
            .join(relative(platform)?)
            .join(relative(component)?)
            .join(relative(&chosen.version.name)?);
        // Do not replace a functional runtime with an unverified one.
        let manifest: RuntimeManifest = self.get_json(
            &chosen.manifest,
            &self.root.join("java/manifests").join(format!(
                "{}-{}-{}.json",
                platform, component, chosen.manifest.sha1
            )),
        )?;
        let executable = path.join(if platform.starts_with("windows") {
            "bin/javaw.exe"
        } else {
            "bin/java"
        });
        let prepared = path.join(".complete");
        if fs::read_to_string(&prepared).is_ok_and(|s| s.trim() == major.to_string())
            && manifest.files.iter().all(|(name, entry)| {
                if entry.kind != "file" {
                    return true;
                }
                let Some(raw) = entry.downloads.as_ref().and_then(|d| d.raw.as_ref()) else {
                    return false;
                };
                let Ok(name) = relative(name) else {
                    return false;
                };
                verify(&path.join(name), raw).unwrap_or(false)
            })
            && fs::symlink_metadata(&executable)
                .is_ok_and(|m| m.file_type().is_file() && !m.file_type().is_symlink())
        {
            return Ok(executable);
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Missing Java directory"))?;
        ensure_dir(parent)?;
        let staged = tempfile::Builder::new()
            .prefix(".java-")
            .tempdir_in(parent)?;
        for (name, entry) in &manifest.files {
            let relative = relative(name)?;
            let dest = staged.path().join(relative);
            match entry.kind.as_str() {
                "directory" => ensure_dir(&dest)?,
                "file" => {
                    let raw = entry
                        .downloads
                        .as_ref()
                        .and_then(|d| d.raw.as_ref())
                        .ok_or_else(|| invalid("Java file has no raw download"))?;
                    self.download(raw, &dest)?;
                    #[cfg(unix)]
                    if entry.executable.unwrap_or(false) {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
                    }
                }
                // Reject links: platform runtimes used by P2 (Windows x64) contain none.
                _ => return Err(invalid("Java manifest contains unsupported link")),
            }
        }
        if !staged
            .path()
            .join(if platform.starts_with("windows") {
                "bin/javaw.exe"
            } else {
                "bin/java"
            })
            .exists()
        {
            return Err(invalid("Java executable absent"));
        }
        fs::write(staged.path().join(".complete"), format!("{}\n", major))?;
        if path.exists() {
            let meta = fs::symlink_metadata(&path)?;
            if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
                return Err(invalid("Java destination not a regular directory"));
            }
            #[cfg(windows)]
            if std::os::windows::fs::MetadataExt::file_attributes(&meta) & 0x400 != 0 {
                return Err(invalid("Java destination is a reparse point"));
            }
            fs::remove_dir_all(&path)?;
        }
        fs::rename(staged.path(), &path)?;
        Ok(executable)
    }
}
#[derive(Debug)]
pub struct VersionResult {
    pub id: String,
    pub java: JavaVersion,
    pub assets: usize,
    pub libraries: usize,
}
#[derive(Debug, Deserialize)]
pub struct JavaVersion {
    pub component: String,
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}
#[derive(Deserialize)]
struct VersionIndex {
    versions: Vec<VersionEntry>,
}
#[derive(Deserialize)]
struct VersionEntry {
    id: String,
    url: String,
    sha1: String,
}
#[derive(Deserialize)]
struct VersionDetail {
    id: String,
    #[serde(rename = "javaVersion")]
    java_version: JavaVersion,
    downloads: VersionDownloads,
    #[serde(rename = "assetIndex")]
    asset_index: Artifact,
    assets: String,
    libraries: Vec<Library>,
}
#[derive(Deserialize)]
struct VersionDownloads {
    client: Artifact,
}
#[derive(Deserialize)]
struct AssetIndex {
    objects: HashMap<String, Asset>,
}
#[derive(Deserialize)]
struct Asset {
    hash: String,
    size: u64,
}
#[derive(Deserialize)]
struct Library {
    downloads: Option<LibraryDownloads>,
    rules: Option<Vec<LibraryRule>>,
    natives: Option<HashMap<String, String>>,
}
#[derive(Deserialize)]
struct LibraryDownloads {
    artifact: Option<LibraryArtifact>,
    classifiers: Option<HashMap<String, LibraryArtifact>>,
}
#[derive(Deserialize)]
struct LibraryArtifact {
    path: String,
    url: String,
    sha1: String,
    size: u64,
}
impl LibraryArtifact {
    fn as_artifact(&self) -> Artifact {
        Artifact {
            url: self.url.clone(),
            sha1: self.sha1.clone(),
            size: self.size,
        }
    }
}
#[derive(Deserialize)]
struct LibraryRule {
    action: String,
    os: Option<OsRule>,
}
#[derive(Deserialize)]
struct OsRule {
    name: Option<String>,
    arch: Option<String>,
}
impl Library {
    fn allowed(&self, platform: &str) -> bool {
        let Some(rules) = &self.rules else {
            return true;
        };
        let mut allowed = false;
        for rule in rules {
            if rule.os.as_ref().is_none_or(|o| {
                o.name.as_deref().is_none_or(|n| n == platform)
                    && o.arch.as_deref().is_none_or(|a| {
                        if cfg!(target_pointer_width = "64") {
                            a == "x64"
                        } else {
                            a == "x86"
                        }
                    })
            }) {
                allowed = rule.action == "allow";
            }
        }
        allowed
    }
}
#[derive(Deserialize)]
struct RuntimeEntry {
    manifest: Artifact,
    version: RuntimeVersion,
}
#[derive(Deserialize)]
struct RuntimeVersion {
    name: String,
}
#[derive(Deserialize)]
struct RuntimeManifest {
    files: HashMap<String, RuntimeFile>,
}
#[derive(Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    downloads: Option<RuntimeDownloads>,
    #[cfg_attr(not(unix), allow(dead_code))]
    executable: Option<bool>,
}
#[derive(Deserialize)]
struct RuntimeDownloads {
    raw: Option<Artifact>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    #[derive(Clone)]
    struct Mem(Arc<Mutex<HashMap<String, Vec<u8>>>>);
    impl Mem {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(HashMap::new())))
        }
        fn add(&self, url: &str, data: &[u8]) -> Artifact {
            self.0.lock().unwrap().insert(url.into(), data.to_vec());
            Artifact {
                url: url.into(),
                sha1: format!("{:x}", Sha1::digest(data)),
                size: data.len() as u64,
            }
        }
    }
    impl Fetch for Mem {
        fn open(&self, url: &str) -> io::Result<Box<dyn Read>> {
            let data = self
                .0
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| invalid("Missing test URL"))?;
            Ok(Box::new(io::Cursor::new(data)))
        }
    }
    #[test]
    fn verified_download_reuses_and_repairs() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let a = m.add("file", b"good");
        let store = Installer {
            fetcher: m.clone(),
            root: t.path().into(),
        };
        let path = t.path().join("x");
        store.download(&a, &path).unwrap();
        m.0.lock().unwrap().remove("file");
        store.download(&a, &path).unwrap();
        fs::write(&path, b"bad").unwrap();
        assert!(store.download(&a, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad");
        m.add("file", b"good");
        store.download(&a, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"good");
    }
    #[test]
    fn mismatch_never_publishes() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let mut a = m.add("file", b"bad");
        a.sha1 = format!("{:x}", Sha1::digest(b"good"));
        let store = Installer {
            fetcher: m,
            root: t.path().into(),
        };
        let path = t.path().join("one");
        assert!(store.download(&a, &path).is_err());
        assert!(!path.exists());
        assert!(relative("../outside").is_err());
        assert!(relative("C:\\Windows").is_err());
    }
    #[test]
    fn windows_library_rules() {
        let lib = Library {
            downloads: None,
            natives: None,
            rules: Some(vec![LibraryRule {
                action: "allow".into(),
                os: Some(OsRule {
                    name: Some("windows".into()),
                    arch: None,
                }),
            }]),
        };
        assert!(lib.allowed("windows"));
        assert!(!lib.allowed("linux"));
    }
    #[test]
    fn p2_version_assets_libraries_fixture() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let client = m.add("client", b"client-jar");
        let asset = m.add("asset", b"asset-data");
        m.add(
            &format!("{}/{}/{}", ASSETS_HOST, &asset.sha1[..2], asset.sha1),
            b"asset-data",
        );
        let library = m.add("library", b"library-jar");
        let asset_index =
            serde_json::json!({"objects":{"audio/example":{"hash":asset.sha1,"size":asset.size}}});
        let assets_json = m.add("assets-index", asset_index.to_string().as_bytes());
        let version = serde_json::json!({
            "id":"1.21.1", "javaVersion":{"component":"java-runtime-delta","majorVersion":21},
            "downloads":{"client":client},"assets":"17","assetIndex":assets_json,
            "libraries":[{"downloads":{"artifact":{"path":"org/example/lib.jar","url":library.url,"sha1":library.sha1,"size":library.size}},"rules":[{"action":"allow","os":{"name":"windows"}}]}]
        });
        let version_artifact = m.add("version", version.to_string().as_bytes());
        m.add(VERSIONS_URL,serde_json::json!({"versions":[{"id":"1.21.1","url":"version","sha1":version_artifact.sha1}]}).to_string().as_bytes());
        let store = Installer {
            fetcher: m.clone(),
            root: t.path().join("managed"),
        };
        let result = store.install_version("1.21.1", "windows").unwrap();
        assert_eq!(result.java.major_version, 21);
        assert_eq!(result.java.component, "java-runtime-delta");
        assert_eq!((result.assets, result.libraries), (1, 1));
        assert_eq!(
            fs::read(store.root.join("versions/1.21.1/client.jar")).unwrap(),
            b"client-jar"
        );
        assert_eq!(
            fs::read(store.root.join("libraries/org/example/lib.jar")).unwrap(),
            b"library-jar"
        );
        assert_eq!(
            fs::read(store.root.join(format!(
                "assets/objects/{}/{}",
                &asset.sha1[..2],
                asset.sha1
            )))
            .unwrap(),
            b"asset-data"
        );
        m.0.lock().unwrap().remove("client");
        store.install_version("1.21.1", "windows").unwrap();
    }
    #[test]
    fn p2_managed_java_fixture_and_reuse() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let exe = m.add("java-exe", b"fake-javabin");
        let manifest = serde_json::json!({"files":{"bin":{"type":"directory"},"bin/javaw.exe":{"type":"file","executable":true,"downloads":{"raw":exe}}}});
        let artifact = m.add("java-manifest", manifest.to_string().as_bytes());
        m.add(RUNTIMES_URL,serde_json::json!({"windows-x64":{"java-runtime-delta":[{"manifest":artifact,"version":{"name":"21.0.7"}}]}}).to_string().as_bytes());
        let store = Installer {
            fetcher: m.clone(),
            root: t.path().join("runtime-data"),
        };
        let exe_path = store
            .install_java("windows-x64", "java-runtime-delta", 21)
            .unwrap();
        assert_eq!(fs::read(&exe_path).unwrap(), b"fake-javabin");
        assert!(exe_path.starts_with(t.path()));
        m.0.lock().unwrap().remove("java-exe");
        assert_eq!(
            store
                .install_java("windows-x64", "java-runtime-delta", 21)
                .unwrap(),
            exe_path
        );
        assert_eq!(
            fs::read_to_string(
                exe_path
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(".complete")
            )
            .unwrap()
            .trim(),
            "21"
        );
    }
    #[test]
    fn p2_corrupt_java_is_reinstalled() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let exe = m.add("java-exe", b"correct");
        let manifest = serde_json::json!({"files":{"bin":{"type":"directory"},"bin/javaw.exe":{"type":"file","downloads":{"raw":exe}}}});
        let artifact = m.add("java-manifest", manifest.to_string().as_bytes());
        m.add(RUNTIMES_URL,serde_json::json!({"windows-x64":{"java-runtime-delta":[{"manifest":artifact,"version":{"name":"21.0.7"}}]}}).to_string().as_bytes());
        let store = Installer {
            fetcher: m,
            root: t.path().join("managed"),
        };
        let path = store
            .install_java("windows-x64", "java-runtime-delta", 21)
            .unwrap();
        fs::write(&path, b"damaged").unwrap();
        store
            .install_java("windows-x64", "java-runtime-delta", 21)
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), b"correct");
    }
    #[test]
    fn p2_java_invalid_entry_rejected_without_installing() {
        let t = tempfile::tempdir().unwrap();
        let m = Mem::new();
        let manifest = serde_json::json!({"files":{"../escape":{"type":"directory"}}});
        let artifact = m.add("bad-java-manifest", manifest.to_string().as_bytes());
        m.add(RUNTIMES_URL,serde_json::json!({"windows-x64":{"java-runtime-delta":[{"manifest":artifact,"version":{"name":"21.0.7"}}]}}).to_string().as_bytes());
        let store = Installer {
            fetcher: m,
            root: t.path().join("managed"),
        };
        assert!(
            store
                .install_java("windows-x64", "java-runtime-delta", 21)
                .is_err()
        );
        assert!(!t.path().join("escape").exists());
        assert!(
            !t.path()
                .join("managed/java/windows-x64/java-runtime-delta/21.0.7")
                .exists()
        );
    }
}
