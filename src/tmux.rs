//! Everything that talks to tmux. The core never sees any of this.

use std::process::Command;

pub struct PaneInfo {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub cursor_x: u16,
    pub cursor_y: u16,
    /// Lines of scrollback above the visible screen.
    pub history: u32,
}

pub fn tmux(args: &[&str]) -> Result<String, String> {
    let out = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run tmux: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn pane_info(target: &str) -> Result<PaneInfo, String> {
    let fmt = "#{pane_id} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{cursor_x} #{cursor_y} #{history_size}";
    let out = tmux(&["display-message", "-p", "-t", target, fmt])?;
    let n: Vec<&str> = out.split_whitespace().collect();
    if n.len() != 8 {
        return Err(format!("unexpected display-message output: {out:?}"));
    }
    let num = |i: usize| {
        n[i].parse::<u16>()
            .map_err(|e| format!("bad number {:?}: {e}", n[i]))
    };
    Ok(PaneInfo {
        left: num(1)?,
        top: num(2)?,
        width: num(3)?,
        height: num(4)?,
        cursor_x: num(5)?,
        cursor_y: num(6)?,
        history: n[7]
            .parse::<u32>()
            .map_err(|e| format!("bad number {:?}: {e}", n[7]))?,
    })
}

/// Exact visible screen with SGR colors (-e) and trailing spaces preserved (-N).
/// Deliberately NOT -J: we want physical rows, not tmux's idea of joined lines.
pub fn capture(target: &str) -> Result<String, String> {
    tmux(&["capture-pane", "-p", "-e", "-N", "-t", target])
}

/// Like `capture`, plus up to `history` lines of scrollback above the visible screen.
pub fn capture_with_history(target: &str, history: u32) -> Result<String, String> {
    if history == 0 {
        return capture(target);
    }
    let start = format!("-{history}");
    tmux(&["capture-pane", "-p", "-e", "-N", "-S", &start, "-t", target])
}

pub fn message(text: &str) {
    let _ = tmux(&["display-message", text]);
}

/// "tmux 3.4", "tmux 3.2a", "tmux next-3.5" -> (3, 4) / (3, 2) / (3, 5)
pub fn version() -> Option<(u32, u32)> {
    let out = tmux(&["-V"]).ok()?;
    parse_version(&out)
}

fn parse_version(s: &str) -> Option<(u32, u32)> {
    let rest = &s[s.find(|c: char| c.is_ascii_digit())?..];
    let (maj, rest) = rest.split_once('.')?;
    let minor: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((maj.parse().ok()?, minor.parse().ok()?))
}

/// Rows the status bar takes at the TOP of the client (popup y is client-relative).
fn status_top_offset() -> u16 {
    let pos = tmux(&["show-options", "-gqv", "status-position"]).unwrap_or_default();
    let st = tmux(&["show-options", "-gqv", "status"]).unwrap_or_default();
    if pos.trim() == "top" && st.trim() != "off" {
        st.trim().parse::<u16>().unwrap_or(1)
    } else {
        0
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// What `pick` needs to know about a snapshot besides its path.
pub struct PickArgs {
    pub cols: u16,
    pub rows: u16, // history + visible rows in the snapshot
    pub cursor: (u16, u16),
    pub click: bool, // start with the cursor cell selected (mouse launch)
}

/// Open a borderless popup exactly over the pane and run `regionsel pick` on the snapshot
/// that was captured before this call.
pub fn popup(
    info: &PaneInfo,
    exe: &str,
    snapshot: &std::path::Path,
    pick: &PickArgs,
) -> Result<(), String> {
    match version() {
        Some(v) if v >= (3, 3) => {}
        Some((a, b)) => {
            return Err(format!(
                "needs tmux >= 3.3 for display-popup -B (found {a}.{b})"
            ))
        }
        None => return Err("cannot parse `tmux -V`".into()),
    }
    let cmd = format!(
        "{} pick --snapshot {} --size {}x{} --cursor {},{}{}",
        sh_quote(exe),
        sh_quote(&snapshot.to_string_lossy()),
        pick.cols,
        pick.rows,
        pick.cursor.0,
        pick.cursor.1,
        if pick.click { " --click" } else { "" }
    );
    let (x, y) = (
        info.left.to_string(),
        (info.top + status_top_offset()).to_string(),
    );
    let (w, h) = (info.width.to_string(), info.height.to_string());
    tmux(&[
        "display-popup",
        "-B",
        "-E",
        "-x",
        &x,
        "-y",
        &y,
        "-w",
        &w,
        "-h",
        &h,
        &cmd,
    ])
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("tmux 3.4\n"), Some((3, 4)));
        assert_eq!(parse_version("tmux 3.2a\n"), Some((3, 2)));
        assert_eq!(parse_version("tmux next-3.5\n"), Some((3, 5)));
        assert_eq!(parse_version("garbage"), None);
    }
}
