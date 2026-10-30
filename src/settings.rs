//! Only the font preference is persisted in P0; no Minecraft data is accessed.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const DEFAULT_FONT: &str = "メイリオ";

pub fn validate_font(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

pub fn config_path() -> io::Result<PathBuf> {
    directories::ProjectDirs::from("", "Cinder", "Cinder Launcher")
        .map(|dirs| dirs.config_dir().join("ui-font.txt"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "設定ディレクトリを取得できません"))
}

pub fn load(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(value) if validate_font(value.trim_end_matches('\n')) => {
            Ok(value.trim_end_matches('\n').to_owned())
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "フォント設定が不正です",
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DEFAULT_FONT.to_owned()),
        Err(err) => Err(err),
    }
}

pub fn save(path: &Path, font: &str) -> io::Result<()> {
    if !validate_font(font) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "フォント名が不正です",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "設定パスが不正です"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, font)?;
    fs::rename(temporary, path)
}

pub fn resolve_font<'a>(preferred: &'a str, installed: &'a [String]) -> &'a str {
    if installed
        .iter()
        .any(|name| name.eq_ignore_ascii_case(preferred))
    {
        return preferred;
    }
    // Japanese-capable families first. Actual glyph coverage must still be tested on each OS.
    for candidate in [
        "Noto Sans CJK JP",
        "Noto Sans JP",
        "Source Han Sans JP",
        "メイリオ",
        "Meiryo UI",
        "Yu Gothic UI",
        "Yu Gothic",
        "Noto Sans",
    ] {
        if let Some(found) = installed
            .iter()
            .find(|name| name.eq_ignore_ascii_case(candidate))
        {
            return found;
        }
    }
    ".SystemUIFont"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/font.txt");
        assert_eq!(load(&path).unwrap(), DEFAULT_FONT);
        save(&path, "Noto Sans CJK JP").unwrap();
        assert_eq!(load(&path).unwrap(), "Noto Sans CJK JP");
        assert!(save(&path, "bad\nfont").is_err());
        assert_eq!(load(&path).unwrap(), "Noto Sans CJK JP");
    }
    #[test]
    fn fallback_if_font_missing() {
        let available = vec!["Noto Sans CJK JP".to_owned()];
        assert_eq!(resolve_font(DEFAULT_FONT, &available), "Noto Sans CJK JP");
        assert_eq!(resolve_font(DEFAULT_FONT, &[]), ".SystemUIFont");
        assert_eq!(resolve_font(DEFAULT_FONT, &["メイリオ".into()]), "メイリオ");
        assert_eq!(
            resolve_font(DEFAULT_FONT, &["Meiryo UI".into()]),
            "Meiryo UI"
        );
        assert_eq!(
            resolve_font("Noto Sans CJK JP", &available),
            "Noto Sans CJK JP"
        );
    }
}
