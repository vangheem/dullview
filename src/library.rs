//! Where presentations come from: folders, archives (`.zip`, `.tar`, `.tar.gz`, `.tgz`,
//! `.tar.bz2`, `.tar.xz`, …), and the list of recently opened ones.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 20;

/// File extensions `File → Open Archive…` and drops accept.
pub const ARCHIVE_EXTS: &[&str] = &["zip", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    /// The folder or archive as the user opened it.
    pub path: PathBuf,
    /// Seconds since the Unix epoch.
    pub opened: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub recent: Vec<Recent>,
    /// `~/Applications/dullview.app` was made once; don't remake it if the user removed it.
    #[serde(default)]
    pub app_installed: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `$XDG_CONFIG_HOME/dullview` or `~/.config/dullview`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("dullview")
}

fn cache_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".cache"))
    };
    base.join("dullview")
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Config {
        fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = fs::write(path, json);
        }
    }

    /// Puts `path` first in the recent list.
    pub fn remember(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|r| r.path != path);
        let opened = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.recent.insert(0, Recent { path, opened });
        self.recent.truncate(MAX_RECENT);
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|r| r.path != path);
    }
}

pub fn is_archive(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ARCHIVE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The deck folder for a folder or archive. An archive is unpacked once into the cache, again
/// whenever it changes; a single top-level folder inside it is the deck.
pub fn resolve(path: &Path) -> Result<PathBuf, String> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    if !path.exists() {
        return Err(format!("{} doesn't exist", path.display()));
    }
    if !is_archive(path) {
        return Err(format!(
            "{} isn't a folder or an archive (.zip, .tar, .tar.gz, …)",
            path.display()
        ));
    }
    let dest = cache_dir().join("archives").join(archive_key(path));
    if !dest.join(".dullview-unpacked").exists() {
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        unpack(path, &dest)?;
        let _ = fs::write(dest.join(".dullview-unpacked"), b"");
    }
    Ok(deck_root(&dest))
}

/// Changes when the archive's path, size or modification time does.
fn archive_key(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let meta = fs::metadata(path).ok();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .hash(&mut h);
    meta.as_ref().map(|m| m.len()).hash(&mut h);
    meta.and_then(|m| m.modified().ok()).hash(&mut h);
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("deck")
        .split('.')
        .next()
        .unwrap_or("deck")
        .to_string();
    format!("{stem}-{:016x}", h.finish())
}

/// The archive's modification time, to notice it changing while it's open.
pub fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn unpack(archive: &Path, dest: &Path) -> Result<(), String> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "zip" {
        let file = fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        let mut zip =
            zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", archive.display()))?;
        // `extract` refuses entries that would land outside `dest`.
        return zip
            .extract(dest)
            .map_err(|e| format!("{}: {e}", archive.display()));
    }
    // The system tar reads every compression it knows (gzip, bzip2, xz, zstd) on its own, and
    // keeps entries inside `dest`.
    let out = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| format!("couldn't run tar: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `dir`, or the single folder inside it when it holds no slides of its own.
fn deck_root(dir: &Path) -> PathBuf {
    let mut dir = dir.to_path_buf();
    for _ in 0..3 {
        let entries: Vec<PathBuf> = fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        let visible = |p: &&PathBuf| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with('.') && n != "__MACOSX")
        };
        let has_slides = entries
            .iter()
            .filter(visible)
            .any(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"));
        let dirs: Vec<&PathBuf> = entries
            .iter()
            .filter(visible)
            .filter(|p| p.is_dir())
            .collect();
        if has_slides || dirs.len() != 1 {
            break;
        }
        dir = dirs[0].clone();
    }
    dir
}

/// A friendly name for a presentation: its folder or archive name.
pub fn display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    for ext in [".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".zip", ".tar"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dullview-lib-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn opens_a_tar_gz_with_a_top_level_folder() {
        let dir = tmp("targz");
        let deck = dir.join("talk");
        crate::init::run(&deck).unwrap();
        let archive = dir.join("talk.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir)
            .arg("talk")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let root = resolve(&archive).unwrap();
        assert_eq!(root.file_name().unwrap(), "talk");
        assert!(!crate::deck::load(&root).slides.is_empty());
        assert_eq!(display_name(&archive), "talk");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn recent_list_moves_reopened_decks_first() {
        let mut c = Config::default();
        c.remember(Path::new("/a"));
        c.remember(Path::new("/b"));
        c.remember(Path::new("/a"));
        let paths: Vec<_> = c.recent.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, [PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
