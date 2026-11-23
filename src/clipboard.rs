//! Clipboard backend chain. First matching local tool wins; the tmux paste buffer
//! (and OSC 52 to the outer terminal, via `load-buffer -w`) is always attempted too,
//! so it also works over SSH.
//!
//! Gotcha on Wayland: tmux's default `update-environment` does not include
//! WAYLAND_DISPLAY, so popups may not see it. `regionsel init` prints the fix.

use std::env;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn have(cmd: &str) -> bool {
    env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| Path::new(&d).join(cmd).is_file()))
        .unwrap_or(false)
}

fn pipe(cmd: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null()) // xclip/wl-copy fork and keep these open; never wait on them
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    } // stdin dropped here -> EOF
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Returns the names of the backends that accepted the text.
pub fn copy(text: &str) -> Vec<&'static str> {
    let mut ok = Vec::new();
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = env::var_os("DISPLAY").is_some();

    if wayland && have("wl-copy") && pipe("wl-copy", &[], text) {
        ok.push("wl-copy");
    } else if x11 && have("xclip") && pipe("xclip", &["-selection", "clipboard"], text) {
        ok.push("xclip");
    } else if x11 && have("xsel") && pipe("xsel", &["--clipboard", "--input"], text) {
        ok.push("xsel");
    } else if have("pbcopy") && pipe("pbcopy", &[], text) {
        ok.push("pbcopy");
    } else if have("clip.exe") && pipe("clip.exe", &[], text) {
        ok.push("clip.exe");
    }

    // -w (tmux >= 3.2) also pushes to the outer terminal via OSC 52; fall back without it.
    if pipe("tmux", &["load-buffer", "-w", "-"], text) || pipe("tmux", &["load-buffer", "-"], text) {
        ok.push("tmux");
    }
    ok
}
