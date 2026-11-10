//! The two app settings "Start at login" and "Start minimised". Both live in the user's autostart entry
//! `$XDG_CONFIG_HOME/autostart/wavexlr.desktop`, so the desktop itself honours them and no extra settings file exists.
//!
//! - Start at login is on when the entry exists and is not switched off (`Hidden=true`, or
//!   `X-GNOME-Autostart-enabled=false`).
//! - Start minimised is on when the `Exec` line ends in `--hidden`.
//! - Without a user entry (a system package installs one in `/etc/xdg/autostart`, which starts hidden) both read as on.
//!   The first change writes the user entry, which overrides the system one.

use crate::ipc::SettingsDto;
use std::io;
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("../packaging/wavexlr-autostart.desktop");
const GROUP: &str = "[Desktop Entry]";
const HIDDEN_FLAG: &str = "--hidden";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    StartAtLogin(bool),
    StartMinimised(bool),
}

pub fn entry_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("autostart").join("wavexlr.desktop")
}

// --- reading and rewriting the text ------------------------------------------------------------

/// The lines of the `[Desktop Entry]` group as a range of line indexes (the whole file if it has no group header).
fn group_range(lines: &[String]) -> std::ops::Range<usize> {
    let start = lines.iter().position(|l| l.trim() == GROUP).map_or(0, |i| i + 1);
    let end = lines[start..].iter().position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |i| start + i);
    start..end
}

fn value_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (k, v) = line.split_once('=')?;
    (k.trim() == key).then_some(v.trim())
}

fn find_key(lines: &[String], key: &str) -> Option<usize> {
    group_range(lines).find(|&i| value_of(&lines[i], key).is_some())
}

fn exec_is_minimised(exec: &str) -> bool {
    exec.trim_end().rsplit_once(char::is_whitespace).is_some_and(|(_, last)| last == HIDDEN_FLAG)
}

fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// What a desktop entry says. An entry without an `Exec` line reads as minimised (the default).
pub fn parse(text: &str) -> SettingsDto {
    let lines = split_lines(text);
    let flag = |key: &str, wanted: &str| {
        group_range(&lines).any(|i| value_of(&lines[i], key).is_some_and(|v| v.eq_ignore_ascii_case(wanted)))
    };
    let exec = find_key(&lines, "Exec").and_then(|i| value_of(&lines[i], "Exec"));
    SettingsDto {
        start_at_login: !(flag("Hidden", "true") || flag("X-GNOME-Autostart-enabled", "false")),
        start_minimised: exec.is_none_or(exec_is_minimised),
    }
}

/// Sets `key=value` inside the group: replaces the line if there is one, else adds it at the group's end.
fn set_key(lines: &mut Vec<String>, key: &str, value: &str) {
    let line = format!("{key}={value}");
    match find_key(lines, key) {
        Some(i) => lines[i] = line,
        None => {
            let range = group_range(lines);
            let mut at = range.end;
            while at > range.start && lines[at - 1].trim().is_empty() {
                at -= 1; // keep blank lines between groups where they are
            }
            lines.insert(at, line);
        }
    }
}

fn remove_key(lines: &mut Vec<String>, key: &str) {
    while let Some(i) = find_key(lines, key) {
        lines.remove(i);
    }
}

fn edit(text: &str, change: impl FnOnce(&mut Vec<String>)) -> String {
    let mut lines = split_lines(text);
    change(&mut lines);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Switches the entry on or off, keeping every other key.
pub fn with_start_at_login(text: &str, on: bool) -> String {
    edit(text, |lines| {
        if on {
            remove_key(lines, "Hidden");
            set_key(lines, "X-GNOME-Autostart-enabled", "true");
        } else {
            set_key(lines, "Hidden", "true");
            set_key(lines, "X-GNOME-Autostart-enabled", "false");
        }
    })
}

/// Adds or removes the trailing `--hidden` of the `Exec` line. `exec` is the command to use if there is no line yet.
pub fn with_start_minimised(text: &str, on: bool, exec: &str) -> String {
    edit(text, |lines| {
        let current = find_key(lines, "Exec").and_then(|i| value_of(&lines[i], "Exec").map(str::to_string));
        let next = match current {
            None if on => format!("{exec} {HIDDEN_FLAG}"),
            None => exec.to_string(),
            Some(current) => match (on, exec_is_minimised(&current)) {
                (true, false) => format!("{} {HIDDEN_FLAG}", current.trim_end()),
                (false, true) => current.trim_end().strip_suffix(HIDDEN_FLAG).unwrap_or(&current).trim_end().to_string(),
                _ => return,
            },
        };
        set_key(lines, "Exec", &next);
    })
}

/// The packaged autostart entry with `Exec` pointing at `exe`.
pub fn template(exe: &Path) -> String {
    let exec = format!("Exec={} {HIDDEN_FLAG}", quote_exec(&exe.to_string_lossy()));
    let mut lines = split_lines(TEMPLATE);
    match lines.iter().position(|l| l.starts_with("Exec=")) {
        Some(i) => lines[i] = exec,
        None => lines.push(exec),
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// An Exec argument as the Desktop Entry spec wants it: quoted when it has spaces or shell characters.
fn quote_exec(arg: &str) -> String {
    if !arg.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c)) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"), // escaped twice: once for the quoting, once for the key syntax
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- the file ----------------------------------------------------------------------------------

/// Reads the settings from an entry file; a missing or unreadable file reads as the defaults (both on).
pub fn read_at(path: &Path) -> SettingsDto {
    std::fs::read_to_string(path).map_or_else(|_| SettingsDto::default(), |text| parse(&text))
}

pub fn read() -> SettingsDto {
    read_at(&entry_path())
}

/// Applies one change to the entry at `path`, creating it from the packaged one for `exe` if it is missing.
/// Written atomically (temporary file, then rename).
pub fn change_at(path: &Path, exe: &Path, change: Change) -> io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let text = existing.clone().unwrap_or_else(|| template(exe));
    let next = match change {
        Change::StartAtLogin(on) => with_start_at_login(&text, on),
        Change::StartMinimised(on) => with_start_minimised(&text, on, &quote_exec(&exe.to_string_lossy())),
    };
    if existing.as_deref() == Some(next.as_str()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("desktop.tmp");
    std::fs::write(&temporary, next)?;
    std::fs::rename(&temporary, path)
}

pub fn change(change: Change, exe: &Path) -> io::Result<()> {
    change_at(&entry_path(), exe, change)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wavexlr-autostart-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const EXE: &str = "/opt/example/.local/bin/wavexlr";

    fn state(start_at_login: bool, start_minimised: bool) -> SettingsDto {
        SettingsDto { start_at_login, start_minimised }
    }

    #[test]
    fn a_missing_file_reads_as_both_on() {
        let dir = temp_dir("missing");
        assert_eq!(read_at(&dir.join("autostart/wavexlr.desktop")), state(true, true));
    }

    #[test]
    fn the_template_reads_as_both_on_and_points_at_the_binary() {
        let text = template(Path::new(EXE));
        assert_eq!(parse(&text), state(true, true));
        assert!(text.contains(&format!("Exec={EXE} --hidden\n")));
        assert!(text.contains("Icon=wavexlr") && text.contains("X-GNOME-Autostart-enabled=true"));
    }

    #[test]
    fn login_off_and_on_again_keeps_the_other_keys() {
        let dir = temp_dir("login");
        let path = dir.join("autostart/wavexlr.desktop");
        let exe = Path::new(EXE);
        change_at(&path, exe, Change::StartAtLogin(true)).unwrap(); // creates it
        let original = std::fs::read_to_string(&path).unwrap();
        assert_eq!(original, template(exe));

        change_at(&path, exe, Change::StartAtLogin(false)).unwrap();
        let off = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&off), state(false, true));
        assert!(off.contains("Hidden=true\n") && off.contains("X-GNOME-Autostart-enabled=false\n"));
        assert!(!off.contains("X-GNOME-Autostart-enabled=true"));
        for kept in ["Name=Wave XLR", "Icon=wavexlr", "Terminal=false", "X-KDE-autostart-after=panel"] {
            assert!(off.contains(kept), "{kept} lost");
        }
        assert_eq!(read_at(&path), state(false, true));

        change_at(&path, exe, Change::StartAtLogin(true)).unwrap();
        let on = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&on), state(true, true));
        assert!(!on.contains("Hidden"));
        assert_eq!(on, original);
        assert!(!path.with_extension("desktop.tmp").exists(), "temporary file left behind");
    }

    #[test]
    fn minimised_off_and_on_changes_only_the_exec_line() {
        let dir = temp_dir("minimised");
        let path = dir.join("autostart/wavexlr.desktop");
        let exe = Path::new(EXE);
        change_at(&path, exe, Change::StartMinimised(false)).unwrap(); // creates it, then removes --hidden
        let off = std::fs::read_to_string(&path).unwrap();
        assert!(off.contains(&format!("Exec={EXE}\n")), "{off}");
        assert_eq!(read_at(&path), state(true, false));

        change_at(&path, exe, Change::StartMinimised(true)).unwrap();
        let on = std::fs::read_to_string(&path).unwrap();
        assert!(on.contains(&format!("Exec={EXE} --hidden\n")));
        assert_eq!(read_at(&path), state(true, true));
        assert_eq!(on, template(exe));
    }

    #[test]
    fn the_two_settings_do_not_disturb_each_other() {
        let dir = temp_dir("both");
        let path = dir.join("autostart/wavexlr.desktop");
        let exe = Path::new(EXE);
        change_at(&path, exe, Change::StartAtLogin(false)).unwrap();
        change_at(&path, exe, Change::StartMinimised(false)).unwrap();
        assert_eq!(read_at(&path), state(false, false));
        change_at(&path, exe, Change::StartAtLogin(true)).unwrap();
        assert_eq!(read_at(&path), state(true, false));
        change_at(&path, exe, Change::StartMinimised(true)).unwrap();
        assert_eq!(read_at(&path), state(true, true));
    }

    #[test]
    fn changing_an_existing_entry_adds_keys_inside_its_own_group_only() {
        let dir = temp_dir("existing");
        let path = dir.join("autostart/wavexlr.desktop");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = "[Desktop Entry]\nName=Wave XLR\nExec=/opt/wavexlr/bin/wavexlr --hidden\n\n[Desktop Action x]\nExec=other\n";
        std::fs::write(&path, text).unwrap();
        // the Exec of the action group must not count
        assert_eq!(parse(text), state(true, true));
        change_at(&path, Path::new(EXE), Change::StartAtLogin(false)).unwrap();
        let off = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            off,
            "[Desktop Entry]\nName=Wave XLR\nExec=/opt/wavexlr/bin/wavexlr --hidden\nHidden=true\nX-GNOME-Autostart-enabled=false\n\n[Desktop Action x]\nExec=other\n"
        );
        // the existing Exec keeps its own binary path
        change_at(&path, Path::new(EXE), Change::StartMinimised(false)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=/opt/wavexlr/bin/wavexlr\n") && text.contains("Exec=other\n"));
    }

    #[test]
    fn hidden_false_and_a_disabled_flag_are_read_correctly() {
        assert_eq!(parse("[Desktop Entry]\nExec=wavexlr\nHidden=false\n"), state(true, false));
        assert_eq!(parse("[Desktop Entry]\nExec=wavexlr --hidden\nHidden=true\n"), state(false, true));
        assert_eq!(parse("[Desktop Entry]\nExec=wavexlr --hidden\nX-GNOME-Autostart-enabled=false\n"), state(false, true));
        assert_eq!(parse("[Desktop Entry]\nExec=wavexlr --hiddenx\n"), state(true, false));
    }

    #[test]
    fn exec_paths_with_spaces_are_quoted() {
        let text = template(Path::new("/opt/example/my apps/wavexlr"));
        assert!(text.contains("Exec=\"/opt/example/my apps/wavexlr\" --hidden\n"));
        assert_eq!(parse(&text), state(true, true));
    }

    #[test]
    fn the_entry_lives_under_xdg_config_home() {
        // The only test that touches the environment, so it cannot race with the others here.
        let dir = temp_dir("env");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        assert_eq!(entry_path(), dir.join("autostart").join("wavexlr.desktop"));
        assert_eq!(read(), state(true, true));
        change(Change::StartAtLogin(false), Path::new(EXE)).unwrap();
        assert!(dir.join("autostart/wavexlr.desktop").exists());
        assert_eq!(read(), state(false, true));
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
