//! Finding and reading the user's config file. The parsing itself is pure and lives in
//! `regionsel::keymap`; this is only the file-system part.

use std::env;
use std::fs;
use std::path::PathBuf;

use regionsel::keymap::Config;

pub struct Loaded {
    pub config: Config,
    /// Problems found in the user's file. Never fatal: the good lines still apply.
    pub warnings: Vec<String>,
}

/// `$REGIONSEL_CONFIG`, else `$XDG_CONFIG_HOME/regionsel/config`, else `~/.config/regionsel/config`.
fn path() -> Option<PathBuf> {
    if let Some(p) = env::var_os("REGIONSEL_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let base = match env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("regionsel").join("config"))
}

pub fn load() -> Loaded {
    let mut config = Config::defaults();
    let mut warnings = Vec::new();
    if let Some(path) = path() {
        match fs::read_to_string(&path) {
            Ok(text) => warnings.extend(config.apply(&text).into_iter().map(|w| w.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // no file: defaults
            Err(e) => warnings.push(format!("{}: {e}", path.display())),
        }
    }
    Loaded { config, warnings }
}
