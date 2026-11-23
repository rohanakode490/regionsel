//! Key bindings, with no terminal library in sight.
//!
//! The frontend turns whatever its toolkit delivers into a `KeyInput` and feeds it to a
//! `Keymap`, which answers with a core `Key` plus a repeat count. Bindings come from a tiny
//! line-based config format (see `default.conf`, which is also the built-in default, parsed
//! by the same code, so the documentation and the behaviour cannot drift apart).

use crate::Key;

/// The built-in configuration, as text.
pub const DEFAULT_CONFIG: &str = include_str!("default.conf");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyName {
    Char(char),
    Space,
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

/// A physical key press. Shift is folded into the character (`G`, `$`), Alt is ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyInput {
    pub name: KeyName,
    pub ctrl: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageMove {
    HalfDown,
    HalfUp,
    FullDown,
    FullUp,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Key(Key),
    Page(PageMove),
}

/// Action names as written in the config file.
const ACTIONS: &[(&str, Command)] = &[
    ("left", Command::Key(Key::Left)),
    ("down", Command::Key(Key::Down)),
    ("up", Command::Key(Key::Up)),
    ("right", Command::Key(Key::Right)),
    ("word-forward", Command::Key(Key::WordFwd)),
    ("word-back", Command::Key(Key::WordBack)),
    ("line-start", Command::Key(Key::LineStart)),
    ("line-end", Command::Key(Key::LineEnd)),
    ("top", Command::Key(Key::Top)),
    ("bottom", Command::Key(Key::Bottom)),
    ("expand", Command::Key(Key::Expand)),
    ("shrink", Command::Key(Key::Shrink)),
    ("visual-char", Command::Key(Key::VisualChar)),
    ("visual-line", Command::Key(Key::VisualLine)),
    ("visual-block", Command::Key(Key::VisualRect)),
    ("copy", Command::Key(Key::Copy)),
    ("cancel", Command::Key(Key::Cancel)),
    ("half-page-down", Command::Page(PageMove::HalfDown)),
    ("half-page-up", Command::Page(PageMove::HalfUp)),
    ("page-down", Command::Page(PageMove::FullDown)),
    ("page-up", Command::Page(PageMove::FullUp)),
];

pub fn parse_action(name: &str) -> Option<Command> {
    ACTIONS.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
}

/// "j", "G", "$", "ctrl-d", "space", "pagedown", ...
pub fn parse_key(spec: &str) -> Result<KeyInput, String> {
    let lower = spec.to_ascii_lowercase();
    let (ctrl, rest) = match lower.strip_prefix("ctrl-") {
        Some(_) => (true, &spec[5..]),
        None => (false, spec),
    };
    let named = match rest.to_ascii_lowercase().as_str() {
        "space" => Some(KeyName::Space),
        "enter" | "return" => Some(KeyName::Enter),
        "esc" | "escape" => Some(KeyName::Esc),
        "backspace" => Some(KeyName::Backspace),
        "tab" => Some(KeyName::Tab),
        "up" => Some(KeyName::Up),
        "down" => Some(KeyName::Down),
        "left" => Some(KeyName::Left),
        "right" => Some(KeyName::Right),
        "pageup" | "pgup" => Some(KeyName::PageUp),
        "pagedown" | "pgdn" => Some(KeyName::PageDown),
        "home" => Some(KeyName::Home),
        "end" => Some(KeyName::End),
        _ => None,
    };
    if let Some(name) = named {
        return Ok(KeyInput { name, ctrl });
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(KeyInput { name: KeyName::Char(if ctrl { c.to_ascii_lowercase() } else { c }), ctrl }),
        _ => Err(format!("unknown key '{spec}'")),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Warning {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "config line {}: {}", self.line, self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    None,
    /// The keys so far begin a longer binding; wait for more.
    Prefix,
    Exact(Command),
}

#[derive(Clone, Debug)]
pub struct Config {
    /// History lines to capture above the screen.
    pub scrollback: u32,
    bindings: Vec<(Vec<KeyInput>, Command)>,
}

impl Config {
    /// The built-in configuration.
    pub fn defaults() -> Config {
        let mut c = Config { scrollback: 2000, bindings: Vec::new() };
        let warnings = c.apply(DEFAULT_CONFIG);
        debug_assert!(warnings.is_empty(), "built-in config has errors: {warnings:?}");
        c
    }

    /// Apply config text on top of the current state. Bad lines are skipped and reported;
    /// good lines still apply, so one typo never disables copying.
    pub fn apply(&mut self, text: &str) -> Vec<Warning> {
        let mut warnings = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Err(message) = self.apply_line(line) {
                warnings.push(Warning { line: i + 1, message });
            }
        }
        warnings
    }

    fn apply_line(&mut self, line: &str) -> Result<(), String> {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t[0] {
            "bind" => {
                if !(3..=4).contains(&t.len()) {
                    return Err("usage: bind <key> [<key>] <action>".into());
                }
                let keys = t[1..t.len() - 1].iter().map(|k| parse_key(k)).collect::<Result<Vec<_>, _>>()?;
                let action = t[t.len() - 1];
                let cmd = parse_action(action).ok_or_else(|| format!("unknown action '{action}'"))?;
                self.bind(keys, cmd, &t[1..t.len() - 1].join(" "))
            }
            "unbind" => {
                if !(2..=3).contains(&t.len()) {
                    return Err("usage: unbind <key> [<key>]".into());
                }
                let keys = t[1..].iter().map(|k| parse_key(k)).collect::<Result<Vec<_>, _>>()?;
                let before = self.bindings.len();
                self.bindings.retain(|(seq, _)| *seq != keys);
                if self.bindings.len() == before {
                    return Err(format!("'{}' is not bound", t[1..].join(" ")));
                }
                Ok(())
            }
            "scrollback" => {
                if t.len() != 2 {
                    return Err("usage: scrollback <lines>".into());
                }
                self.scrollback = t[1].parse().map_err(|_| format!("'{}' is not a number of lines", t[1]))?;
                Ok(())
            }
            other => Err(format!("unknown directive '{other}' (expected bind, unbind or scrollback)")),
        }
    }

    fn bind(&mut self, keys: Vec<KeyInput>, cmd: Command, shown: &str) -> Result<(), String> {
        if let [KeyInput { name: KeyName::Char('1'..='9'), ctrl: false }, ..] = keys[..] {
            return Err(format!("'{shown}': digits 1-9 start a count (as in 5j) and cannot be bound"));
        }
        if let Some(existing) = self.bindings.iter_mut().find(|(seq, _)| *seq == keys) {
            existing.1 = cmd; // rebinding the same keys replaces
            return Ok(());
        }
        let clash = self.bindings.iter().any(|(seq, _)| seq.starts_with(&keys) || keys.starts_with(seq));
        if clash {
            return Err(format!(
                "'{shown}' conflicts with an existing binding (a key cannot be both a binding and the start of a sequence); unbind the other one first"
            ));
        }
        self.bindings.push((keys, cmd));
        Ok(())
    }

    pub fn lookup(&self, seq: &[KeyInput]) -> Lookup {
        if let Some((_, cmd)) = self.bindings.iter().find(|(s, _)| s == seq) {
            return Lookup::Exact(*cmd);
        }
        if self.bindings.iter().any(|(s, _)| s.len() > seq.len() && s.starts_with(seq)) {
            return Lookup::Prefix;
        }
        Lookup::None
    }
}

/// Count prefixes (`5j`) and key sequences (`gg`) on top of a `Config`.
#[derive(Default)]
pub struct Keymap {
    pending: Vec<KeyInput>,
    count: Option<u32>,
}

impl Keymap {
    /// Feed one key press (`None` = a key we have no name for). `rows` is the screen height,
    /// for the page-wise commands. Returns the core key and how many times to apply it.
    pub fn feed(&mut self, cfg: &Config, input: Option<KeyInput>, rows: u32) -> Option<(Key, u32)> {
        let Some(input) = input else {
            self.pending.clear();
            self.count = None;
            return None;
        };
        if self.pending.is_empty() && !input.ctrl {
            if let KeyName::Char(d) = input.name {
                if d.is_ascii_digit() && (d != '0' || self.count.is_some()) {
                    let digit = d.to_digit(10).unwrap_or(0);
                    self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(digit));
                    return None;
                }
            }
        }
        self.pending.push(input);
        loop {
            match cfg.lookup(&self.pending) {
                Lookup::Exact(cmd) => {
                    self.pending.clear();
                    return Some(self.emit(cmd, rows));
                }
                Lookup::Prefix => return None,
                Lookup::None if self.pending.len() > 1 => {
                    // "g" then something that is not "g": drop the dead prefix and treat the
                    // new key on its own.
                    let last = self.pending[self.pending.len() - 1];
                    self.pending.clear();
                    self.pending.push(last);
                }
                Lookup::None => {
                    self.pending.clear();
                    self.count = None;
                    return None;
                }
            }
        }
    }

    fn emit(&mut self, cmd: Command, rows: u32) -> (Key, u32) {
        let count = self.count.take();
        match cmd {
            Command::Key(key) => {
                let n = count.unwrap_or(1).clamp(1, 9999);
                let repeatable = matches!(
                    key,
                    Key::Left | Key::Right | Key::Up | Key::Down | Key::WordFwd | Key::WordBack | Key::Expand | Key::Shrink
                );
                (key, if repeatable { n } else { 1 })
            }
            Command::Page(p) => {
                let half = (rows / 2).max(1);
                let page = rows.saturating_sub(1).max(1); // keep a row of context, like vim
                match p {
                    PageMove::HalfDown => (Key::Down, half),
                    PageMove::HalfUp => (Key::Up, half),
                    PageMove::FullDown => (Key::Down, page),
                    PageMove::FullUp => (Key::Up, page),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(spec: &str) -> Option<KeyInput> {
        Some(parse_key(spec).unwrap())
    }
    fn feed(m: &mut Keymap, c: &Config, specs: &[&str]) -> Vec<Option<(Key, u32)>> {
        specs.iter().map(|s| m.feed(c, k(s), 40)).collect()
    }

    #[test]
    fn parses_key_specs() {
        assert_eq!(parse_key("j").unwrap(), KeyInput { name: KeyName::Char('j'), ctrl: false });
        assert_eq!(parse_key("G").unwrap(), KeyInput { name: KeyName::Char('G'), ctrl: false });
        assert_eq!(parse_key("$").unwrap().name, KeyName::Char('$'));
        assert_eq!(parse_key("ctrl-d").unwrap(), KeyInput { name: KeyName::Char('d'), ctrl: true });
        assert_eq!(parse_key("Ctrl-D").unwrap(), KeyInput { name: KeyName::Char('d'), ctrl: true });
        assert_eq!(parse_key("Space").unwrap().name, KeyName::Space);
        assert_eq!(parse_key("pgdn").unwrap().name, KeyName::PageDown);
        assert_eq!(parse_key("ctrl--").unwrap(), KeyInput { name: KeyName::Char('-'), ctrl: true });
        assert!(parse_key("ctrl-").is_err());
        assert!(parse_key("jj").is_err());
        assert!(parse_key("").is_err());
    }

    #[test]
    fn built_in_defaults_parse_cleanly_and_document_every_action() {
        let mut c = Config { scrollback: 0, bindings: Vec::new() };
        assert_eq!(c.apply(DEFAULT_CONFIG), vec![]);
        assert_eq!(c.scrollback, 2000);
        for (name, _) in ACTIONS {
            assert!(DEFAULT_CONFIG.contains(name), "action '{name}' is not documented in default.conf");
        }
    }

    #[test]
    fn defaults_behave_like_the_original_hardcoded_keys() {
        let c = Config::defaults();
        let mut m = Keymap::default();
        assert_eq!(feed(&mut m, &c, &["j"]), vec![Some((Key::Down, 1))]);
        assert_eq!(feed(&mut m, &c, &["5", "j"]), vec![None, Some((Key::Down, 5))]);
        assert_eq!(feed(&mut m, &c, &["1", "0", "w"]), vec![None, None, Some((Key::WordFwd, 10))]);
        assert_eq!(feed(&mut m, &c, &["0"]), vec![Some((Key::LineStart, 1))]); // no count pending: a command
        assert_eq!(feed(&mut m, &c, &["g", "g"]), vec![None, Some((Key::Top, 1))]);
        assert_eq!(feed(&mut m, &c, &["G", "$", "space", "backspace", "enter"]).iter().flatten().count(), 5);
        assert_eq!(feed(&mut m, &c, &["ctrl-v", "v", "V"]), vec![
            Some((Key::VisualRect, 1)),
            Some((Key::VisualChar, 1)),
            Some((Key::VisualLine, 1))
        ]);
        for q in ["q", "esc", "ctrl-c"] {
            assert_eq!(feed(&mut m, &c, &[q]), vec![Some((Key::Cancel, 1))], "{q}");
        }
        // Counts only repeat motions: "3G" is just G.
        assert_eq!(feed(&mut m, &c, &["3", "G"]), vec![None, Some((Key::Bottom, 1))]);
    }

    #[test]
    fn page_keys_use_the_screen_height() {
        let c = Config::defaults();
        let mut m = Keymap::default();
        assert_eq!(m.feed(&c, k("ctrl-d"), 40), Some((Key::Down, 20)));
        assert_eq!(m.feed(&c, k("ctrl-u"), 40), Some((Key::Up, 20)));
        assert_eq!(m.feed(&c, k("pagedown"), 40), Some((Key::Down, 39)));
        assert_eq!(m.feed(&c, k("ctrl-b"), 1), Some((Key::Up, 1))); // never zero
    }

    #[test]
    fn a_dead_prefix_is_dropped_and_the_next_key_still_counts() {
        let c = Config::defaults();
        let mut m = Keymap::default();
        assert_eq!(feed(&mut m, &c, &["g", "j"]), vec![None, Some((Key::Down, 1))]);
        assert_eq!(feed(&mut m, &c, &["g", "g", "g", "g"]), vec![None, Some((Key::Top, 1)), None, Some((Key::Top, 1))]);
        // An unnamed key resets a pending count.
        assert_eq!(m.feed(&c, k("5"), 40), None);
        assert_eq!(m.feed(&c, None, 40), None);
        assert_eq!(m.feed(&c, k("j"), 40), Some((Key::Down, 1)));
    }

    #[test]
    fn user_config_rebinds_unbinds_and_sets_scrollback() {
        let mut c = Config::defaults();
        let w = c.apply("# my keys\nbind x cancel\nunbind q\nbind j half-page-down\nscrollback 500\nunbind g g\nbind g top\n");
        assert_eq!(w, vec![]);
        assert_eq!(c.scrollback, 500);
        let mut m = Keymap::default();
        assert_eq!(feed(&mut m, &c, &["x"]), vec![Some((Key::Cancel, 1))]);
        assert_eq!(feed(&mut m, &c, &["q"]), vec![None]); // unbound now
        assert_eq!(m.feed(&c, k("j"), 10), Some((Key::Down, 5))); // replaced
        assert_eq!(feed(&mut m, &c, &["g"]), vec![Some((Key::Top, 1))]); // single g, no waiting
        assert_eq!(feed(&mut m, &c, &["esc"]), vec![Some((Key::Cancel, 1))]); // untouched default
    }

    #[test]
    fn bad_lines_warn_with_line_numbers_and_good_lines_still_apply() {
        let mut c = Config::defaults();
        let w = c.apply(
            "bind x cancel\nbind y nonsense\nbind\nfrobnicate 3\nunbind F\nbind g top\nscrollback many\nbind ctrl-zz copy\nbind z copy\n",
        );
        let lines: Vec<usize> = w.iter().map(|w| w.line).collect();
        assert_eq!(lines, vec![2, 3, 4, 5, 6, 7, 8]);
        assert!(w[0].message.contains("unknown action 'nonsense'"));
        assert!(w[3].message.contains("'F' is not bound"));
        assert!(w[4].message.contains("conflicts")); // 'g' vs the default "g g"
        assert_eq!(w[0].to_string(), "config line 2: unknown action 'nonsense'");
        let mut m = Keymap::default();
        assert_eq!(feed(&mut m, &c, &["x", "z"]), vec![Some((Key::Cancel, 1)), Some((Key::Copy, 1))]);
        assert_eq!(c.scrollback, 2000); // the bad scrollback line changed nothing
    }

    #[test]
    fn digits_start_counts_and_cannot_be_bound() {
        let mut c = Config::defaults();
        let w = c.apply("bind 5 cancel\nbind 0 top\n");
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("digits 1-9"));
        let mut m = Keymap::default();
        assert_eq!(feed(&mut m, &c, &["5", "j"]), vec![None, Some((Key::Down, 5))]);
    }
}
