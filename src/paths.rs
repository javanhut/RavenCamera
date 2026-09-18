//! Where things go: the XDG base directories and the user directories
//! (`~/Videos`, `~/Pictures`), resolved the way every Raven component does it
//! so a capture lands where the file manager's sidebar says Videos is.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg(var: &str, fallback: &[&str]) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| fallback.iter().fold(home(), |p, part| p.join(part)))
}

/// `~/.config/raven`, shared with the rest of the desktop.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", &[".config"]).join("raven")
}

/// `~/.local/share/raven-camera`: recordings in progress live here until they
/// are finished, so a crash or a power cut leaves something to recover.
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", &[".local", "share"]).join("raven-camera")
}

/// `~/.cache/raven-camera`: thumbnails, all of which can be made again.
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", &[".cache"]).join("raven-camera")
}

/// Unfinished and finishing recordings.
pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

/// A user directory from `user-dirs.dirs` (`XDG_VIDEOS_DIR` and friends),
/// falling back to `~/<fallback>`.
pub fn user_dir(key: &str, fallback: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(key).map(PathBuf::from) {
        if dir.is_absolute() {
            return dir;
        }
    }
    let file = xdg("XDG_CONFIG_HOME", &[".config"]).join("user-dirs.dirs");
    if let Ok(text) = std::fs::read_to_string(file) {
        if let Some(dir) = parse_user_dirs(&text, key, &home()) {
            return dir;
        }
    }
    home().join(fallback)
}

/// The value of `key` in a `user-dirs.dirs` file: shell syntax, one
/// `XDG_X_DIR="$HOME/Thing"` per line, and only `$HOME/` or an absolute path
/// allowed by the spec.
fn parse_user_dirs(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            if k.trim() != key {
                return None;
            }
            let v = v.trim().trim_matches('"');
            if let Some(rest) = v.strip_prefix("$HOME") {
                let rest = rest.trim_start_matches('/');
                Some(if rest.is_empty() {
                    home.to_path_buf()
                } else {
                    home.join(rest)
                })
            } else if v.starts_with('/') {
                Some(PathBuf::from(v))
            } else {
                None
            }
        })
}

/// `~/Videos`, where recordings go unless the settings say otherwise.
pub fn videos_dir() -> PathBuf {
    user_dir("XDG_VIDEOS_DIR", "Videos")
}

/// `~/Pictures`, where photos and screenshots go unless the settings say
/// otherwise.
pub fn pictures_dir() -> PathBuf {
    user_dir("XDG_PICTURES_DIR", "Pictures")
}

/// `path` with the home directory written as `~`, for showing in the window.
pub fn display(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_expand_home_and_ignore_other_keys() {
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_VIDEOS_DIR=\"$HOME/Filme\"\n";
        let home = Path::new("/home/a");
        assert_eq!(
            parse_user_dirs(text, "XDG_VIDEOS_DIR", home),
            Some(PathBuf::from("/home/a/Filme"))
        );
        assert_eq!(parse_user_dirs(text, "XDG_MUSIC_DIR", home), None);
    }

    #[test]
    fn user_dirs_accept_absolute_paths_and_bare_home() {
        let home = Path::new("/home/a");
        assert_eq!(
            parse_user_dirs("XDG_VIDEOS_DIR=\"/data/v\"", "XDG_VIDEOS_DIR", home),
            Some(PathBuf::from("/data/v"))
        );
        assert_eq!(
            parse_user_dirs("XDG_VIDEOS_DIR=\"$HOME/\"", "XDG_VIDEOS_DIR", home),
            Some(PathBuf::from("/home/a"))
        );
        assert_eq!(
            parse_user_dirs("XDG_VIDEOS_DIR=\"relative\"", "XDG_VIDEOS_DIR", home),
            None
        );
    }
}
