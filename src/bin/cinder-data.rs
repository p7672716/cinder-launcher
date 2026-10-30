//! Internal P2 verifier. No account access or game launch.
use cinder_launcher::minecraft_data::{HttpFetch, Installer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !matches!(args[1].as_str(), "inspect" | "java" | "install") {
        eprintln!("usage: cinder-data <inspect|java|install> <Minecraft version>");
        std::process::exit(2);
    }
    let installer = Installer {
        fetcher: HttpFetch::new()?,
        root: std::env::var_os("CINDER_TEST_DATA_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or(Installer::<HttpFetch>::default_root()?),
    };
    let java = installer.version_java(&args[2])?;
    println!(
        "{}: Java {} ({})",
        args[2], java.major_version, java.component
    );
    if args[1] == "java" {
        let platform = if cfg!(target_os = "windows") {
            "windows-x64"
        } else if cfg!(target_os = "macos") {
            "mac-os"
        } else {
            "linux"
        };
        let path = installer.install_java(platform, &java.component, java.major_version)?;
        println!("Managed Java: {}", path.display());
    } else if args[1] == "install" {
        let platform = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "osx"
        } else {
            "linux"
        };
        let done = installer.install_version(&args[2], platform)?;
        println!(
            "Installed {}: {} assets and {} libraries",
            done.id, done.assets, done.libraries
        );
    }
    Ok(())
}
