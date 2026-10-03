// Copyright (C) 2026 The Argus Capture community
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use configparser::ini::Ini;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SHORTCUTS_SECTION: &str = "ArgusCapture.Shortcuts";
const USER_CONFIG_DIR: &str = ".argus-capture";
const CONFIG_FILE_NAME: &str = "argus-capture.conf";
pub(crate) const DEFAULT_CONFIG_PATH: &str = "/etc/argus-capture/argus-capture.conf";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct AppConfig {
    pub(crate) path: PathBuf,
    workspace: PathBuf,
    storage: StorageMode,
    shortcuts: Shortcuts,
    selected_camera: ConfiguredCamera,
}

impl AppConfig {
    pub(crate) fn load(config_path: Option<&Path>) -> io::Result<Option<Self>> {
        match config_path {
            Some(path) => Self::from_path(path).map(Some),
            None => {
                for path in config_search_paths(current_home_dir().as_deref()) {
                    if path.exists() {
                        return Self::from_path(&path).map(Some);
                    }
                }

                Ok(None)
            }
        }
    }

    pub(crate) fn selected_camera(&self) -> &ConfiguredCamera {
        &self.selected_camera
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn storage(&self) -> StorageMode {
        self.storage
    }

    pub(crate) fn shortcuts(&self) -> &Shortcuts {
        &self.shortcuts
    }

    fn from_path(path: &Path) -> io::Result<Self> {
        let mut ini = Ini::new();
        ini.load(path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Self::from_ini(path.to_path_buf(), ini)
    }

    fn from_ini(path: PathBuf, ini: Ini) -> io::Result<Self> {
        let workspace = optional_value(&ini, "ArgusCapture", "workspace")
            .map(PathBuf::from)
            .unwrap_or_else(default_workspace);
        let storage = match optional_value(&ini, "ArgusCapture", "storage") {
            Some(value) => StorageMode::parse(&value).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid storage mode `{value}` in {}: expected `camera_only`, `workspace_only`, or `both`",
                        path.display()
                    ),
                )
            })?,
            None => default_storage(),
        };
        let camera_name = required_value(&ini, "ArgusCapture", "camera")?;
        let host = required_value(&ini, &camera_name, "host")?;
        let port = required_value(&ini, &camera_name, "port")?
            .parse::<u16>()
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid port for camera section `{camera_name}` in {}: {error}",
                        path.display()
                    ),
                )
            })?;

        let username = optional_value(&ini, &camera_name, "username");
        let password = optional_value(&ini, &camera_name, "password");
        let shortcuts = Shortcuts::from_ini(&ini);

        if username.is_some() != password.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "camera section `{camera_name}` in {} must define both username and password or neither",
                    path.display()
                ),
            ));
        }

        Ok(Self {
            path,
            workspace,
            storage,
            shortcuts,
            selected_camera: ConfiguredCamera {
                name: camera_name,
                host,
                port,
                username,
                password,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageMode {
    CameraOnly,
    WorkspaceOnly,
    Both,
}

impl StorageMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "camera_only" => Some(Self::CameraOnly),
            "workspace_only" => Some(Self::WorkspaceOnly),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::CameraOnly => "camera_only",
            Self::WorkspaceOnly => "workspace_only",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ConfiguredCamera {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
}

impl ConfiguredCamera {
    pub(crate) fn credentials(&self) -> Option<(&str, &str)> {
        Some((self.username.as_deref()?, self.password.as_deref()?))
    }
}

pub(crate) struct ShortcutDef {
    pub(crate) action: &'static str,
    pub(crate) menu: &'static str,
    pub(crate) label: &'static str,
    pub(crate) default: &'static str,
}

impl ShortcutDef {
    pub(crate) fn title(&self) -> String {
        format!("{} > {}", self.menu, self.label)
    }
}

pub(crate) const SHORTCUT_DEFS: &[ShortcutDef] = &[
    ShortcutDef {
        action: "quit",
        menu: "File",
        label: "Quit",
        default: "q",
    },
    ShortcutDef {
        action: "camera-connect",
        menu: "Camera",
        label: "Connect",
        default: "c",
    },
    ShortcutDef {
        action: "camera-disconnect",
        menu: "Camera",
        label: "Disconnect",
        default: "d",
    },
    ShortcutDef {
        action: "camera-capture",
        menu: "Camera",
        label: "Take Picture",
        default: "p",
    },
    ShortcutDef {
        action: "camera-focus",
        menu: "Camera",
        label: "Focus",
        default: "f",
    },
    ShortcutDef {
        action: "edit-configuration",
        menu: "Edit",
        label: "Configuration",
        default: "",
    },
    ShortcutDef {
        action: "album-pictures",
        menu: "Album",
        label: "Pictures",
        default: "",
    },
    ShortcutDef {
        action: "album-videos",
        menu: "Album",
        label: "Videos",
        default: "",
    },
    ShortcutDef {
        action: "help-about",
        menu: "Help",
        label: "About",
        default: "a",
    },
];

pub(crate) fn shortcut_matches(def: &ShortcutDef, accel: &str, query: &str) -> bool {
    let haystack =
        format!("{} {} {}", def.menu, def.label, display_accelerator(accel)).to_lowercase();

    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct Shortcuts {
    bindings: HashMap<&'static str, String>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            bindings: SHORTCUT_DEFS
                .iter()
                .map(|def| (def.action, def.default.to_owned()))
                .collect(),
        }
    }
}

impl Shortcuts {
    fn from_ini(ini: &Ini) -> Self {
        let mut shortcuts = Self::default();

        for def in SHORTCUT_DEFS {
            if let Some(value) = optional_value(ini, SHORTCUTS_SECTION, def.action) {
                let accel = if value.eq_ignore_ascii_case("none") {
                    ""
                } else {
                    value.as_str()
                };
                shortcuts.set(def.action, accel);
            }
        }

        shortcuts
    }

    pub(crate) fn accel(&self, action: &str) -> &str {
        self.bindings.get(action).map_or("", String::as_str)
    }

    pub(crate) fn set(&mut self, action: &str, accel: &str) {
        if let Some(def) = SHORTCUT_DEFS.iter().find(|def| def.action == action) {
            self.bindings.insert(def.action, accel.trim().to_owned());
        }
    }

    /// Returns the first pair of actions that share the same shortcut.
    pub(crate) fn conflict(&self) -> Option<(&'static ShortcutDef, &'static ShortcutDef)> {
        for (index, first) in SHORTCUT_DEFS.iter().enumerate() {
            let first_accel = normalize_accelerator(self.accel(first.action));
            if first_accel.is_empty() {
                continue;
            }

            for second in &SHORTCUT_DEFS[index + 1..] {
                if normalize_accelerator(self.accel(second.action)) == first_accel {
                    return Some((first, second));
                }
            }
        }

        None
    }

    fn render_section(&self) -> String {
        let mut section = format!("[{SHORTCUTS_SECTION}]\n");

        for def in SHORTCUT_DEFS {
            let accel = self.accel(def.action);
            let accel = if accel.is_empty() { "none" } else { accel };
            section.push_str(&format!("{} = {accel}\n", def.action));
        }

        section
    }
}

const MODIFIER_NAMES: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

// Splits a GTK accelerator such as `<Control><Shift>p` into its modifier flags
// (Ctrl, Alt, Shift, Super) and the remaining key name.
fn split_accelerator(accel: &str) -> ([bool; 4], String) {
    let mut modifiers = [false; 4];
    let mut rest = accel.trim();

    while let Some(stripped) = rest.strip_prefix('<') {
        let Some(end) = stripped.find('>') else {
            break;
        };

        match stripped[..end].to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "primary" => modifiers[0] = true,
            "alt" | "mod1" => modifiers[1] = true,
            "shift" => modifiers[2] = true,
            "super" | "meta" | "mod4" => modifiers[3] = true,
            _ => break,
        }

        rest = &stripped[end + 1..];
    }

    (modifiers, rest.trim().to_owned())
}

/// Human-readable form of an accelerator, e.g. `<Control>f` -> `Ctrl+F`.
pub(crate) fn display_accelerator(accel: &str) -> String {
    let (modifiers, key) = split_accelerator(accel);
    if key.is_empty() {
        return "Not set".to_owned();
    }

    let mut parts: Vec<String> = MODIFIER_NAMES
        .iter()
        .zip(modifiers)
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| (*name).to_owned())
        .collect();
    parts.push(if key.chars().count() == 1 {
        key.to_uppercase()
    } else {
        key
    });
    parts.join("+")
}

/// Canonical form used to compare accelerators (`<Primary>F` == `<Control>f`).
pub(crate) fn normalize_accelerator(accel: &str) -> String {
    let (modifiers, key) = split_accelerator(accel);
    if key.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = MODIFIER_NAMES
        .iter()
        .zip(modifiers)
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    parts.push(key.to_ascii_lowercase());
    parts.join("+")
}

pub(crate) fn render_user_config(
    workspace: &Path,
    storage: StorageMode,
    camera: &ConfiguredCamera,
    shortcuts: &Shortcuts,
) -> String {
    let mut config = format!(
        "[ArgusCapture]\n\
         workspace = {workspace}\n\
         storage = {storage}\n\
         camera = {name}\n\n\
         [{name}]\n\
         host = {host}\n\
         port = {port}\n",
        workspace = workspace.display(),
        storage = storage.as_config_value(),
        name = camera.name,
        host = camera.host,
        port = camera.port,
    );

    if let Some(username) = camera.username.as_deref() {
        config.push_str(&format!("username = {username}\n"));
    }

    if let Some(password) = camera.password.as_deref() {
        config.push_str(&format!("password = {password}\n"));
    }

    config.push('\n');
    config.push_str(&shortcuts.render_section());
    config
}

pub(crate) fn default_workspace() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn default_storage() -> StorageMode {
    StorageMode::WorkspaceOnly
}

pub(crate) fn user_config_path() -> io::Result<PathBuf> {
    let Some(home_dir) = current_home_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is not set; cannot create ~/.argus-capture/argus-capture.conf",
        ));
    };

    Ok(home_dir.join(USER_CONFIG_DIR).join(CONFIG_FILE_NAME))
}

pub(crate) fn write_user_config(
    path: &Path,
    workspace: &Path,
    storage: StorageMode,
    camera: &ConfiguredCamera,
    shortcuts: &Shortcuts,
) -> io::Result<()> {
    let rendered = render_user_config(workspace, storage, camera, shortcuts);

    #[cfg(unix)]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(rendered.as_bytes())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::write(path, rendered)?;
        Ok(())
    }
}

fn required_value(ini: &Ini, section: &str, key: &str) -> io::Result<String> {
    optional_value(ini, section, key).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("missing `{key}` in [{section}]"),
        )
    })
}

fn optional_value(ini: &Ini, section: &str, key: &str) -> Option<String> {
    ini.get(section, key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn current_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn config_search_paths(home_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home_dir) = home_dir {
        paths.push(home_dir.join(USER_CONFIG_DIR).join(CONFIG_FILE_NAME));
    }

    paths.push(PathBuf::from(DEFAULT_CONFIG_PATH));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_SHORTCUTS_SECTION: &str = "\n[ArgusCapture.Shortcuts]\n\
         quit = q\n\
         camera-connect = c\n\
         camera-disconnect = d\n\
         camera-capture = p\n\
         camera-focus = f\n\
         edit-configuration = none\n\
         album-pictures = none\n\
         album-videos = none\n\
         help-about = a\n";

    fn parse_config(input: &str) -> io::Result<AppConfig> {
        let mut ini = Ini::new();
        ini.read(input.to_owned())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        AppConfig::from_ini(PathBuf::from("argus-capture.conf"), ini)
    }

    #[test]
    fn parses_selected_camera_configuration() {
        let config = parse_config(
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = both\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n\
             username = abbc\n\
             password = cbbaabbc\n",
        )
        .unwrap();

        assert_eq!(config.workspace(), Path::new("/var/lib/argus/workspace"));
        assert_eq!(config.storage(), StorageMode::Both);
        assert_eq!(config.selected_camera().name, "CanonR3");
        assert_eq!(config.selected_camera().host, "192.168.1.23");
        assert_eq!(config.selected_camera().port, 80);
        assert_eq!(
            config.selected_camera().credentials(),
            Some(("abbc", "cbbaabbc"))
        );
    }

    #[test]
    fn rejects_missing_selected_camera_section() {
        let error = parse_config(
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = workspace_only\n\
             camera = CanonR3\n\n\
             [OtherCamera]\n\
             host = 192.168.1.23\n\
             port = 80\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("missing `host` in [CanonR3]"));
    }

    #[test]
    fn rejects_partial_credentials() {
        let error = parse_config(
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = workspace_only\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n\
             username = abbc\n",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must define both username and password or neither")
        );
    }

    #[test]
    fn defaults_workspace_to_current_directory_when_missing() {
        let config = parse_config(
            "[ArgusCapture]\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n",
        )
        .unwrap();

        assert_eq!(config.workspace(), default_workspace().as_path());
        assert_eq!(config.storage(), default_storage());
    }

    #[test]
    fn rejects_unknown_storage_mode() {
        let error = parse_config(
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = someday\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid storage mode `someday`"));
    }

    #[test]
    fn prefers_home_directory_config_before_system_config() {
        let paths = config_search_paths(Some(Path::new("/home/jesper")));

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/jesper/.argus-capture/argus-capture.conf"),
                PathBuf::from("/etc/argus-capture/argus-capture.conf"),
            ]
        );
    }

    #[test]
    fn falls_back_to_system_config_when_home_directory_is_unknown() {
        let paths = config_search_paths(None);

        assert_eq!(
            paths,
            vec![PathBuf::from("/etc/argus-capture/argus-capture.conf")]
        );
    }

    #[test]
    fn renders_user_config_in_ini_format() {
        let config = render_user_config(
            Path::new("/var/lib/argus/workspace"),
            StorageMode::Both,
            &ConfiguredCamera {
                name: "CanonR3".to_owned(),
                host: "192.168.1.23".to_owned(),
                port: 80,
                username: Some("abbc".to_owned()),
                password: Some("cbbaabbc".to_owned()),
            },
            &Shortcuts::default(),
        );

        assert_eq!(
            config,
            format!(
                "[ArgusCapture]\n\
                 workspace = /var/lib/argus/workspace\n\
                 storage = both\n\
                 camera = CanonR3\n\n\
                 [CanonR3]\n\
                 host = 192.168.1.23\n\
                 port = 80\n\
                 username = abbc\n\
                 password = cbbaabbc\n{DEFAULT_SHORTCUTS_SECTION}"
            )
        );
    }

    #[test]
    fn omits_blank_optional_credentials_from_rendered_config() {
        let config = render_user_config(
            Path::new("/var/lib/argus/workspace"),
            StorageMode::WorkspaceOnly,
            &ConfiguredCamera {
                name: "CanonR3".to_owned(),
                host: "192.168.1.23".to_owned(),
                port: 80,
                username: None,
                password: None,
            },
            &Shortcuts::default(),
        );

        assert_eq!(
            config,
            format!(
                "[ArgusCapture]\n\
                 workspace = /var/lib/argus/workspace\n\
                 storage = workspace_only\n\
                 camera = CanonR3\n\n\
                 [CanonR3]\n\
                 host = 192.168.1.23\n\
                 port = 80\n{DEFAULT_SHORTCUTS_SECTION}"
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_user_config_with_owner_only_permissions() {
        let temp_dir =
            std::env::temp_dir().join(format!("argus-capture-config-test-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let config_path = temp_dir.join("argus-capture.conf");

        write_user_config(
            &config_path,
            Path::new("/var/lib/argus/workspace"),
            StorageMode::WorkspaceOnly,
            &ConfiguredCamera {
                name: "CanonR3".to_owned(),
                host: "camera.example.local".to_owned(),
                port: 443,
                username: None,
                password: None,
            },
            &Shortcuts::default(),
        )
        .unwrap();

        let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_file(&config_path).unwrap();
        fs::remove_dir(&temp_dir).unwrap();
    }

    const MINIMAL_CAMERA: &str = "[ArgusCapture]\n\
         camera = CanonR3\n\n\
         [CanonR3]\n\
         host = 192.168.1.23\n\
         port = 80\n";

    #[test]
    fn uses_default_shortcuts_when_the_section_is_missing() {
        let config = parse_config(MINIMAL_CAMERA).unwrap();

        assert_eq!(config.shortcuts(), &Shortcuts::default());
        assert_eq!(config.shortcuts().accel("quit"), "q");
        assert_eq!(config.shortcuts().accel("camera-connect"), "c");
    }

    #[test]
    fn parses_custom_and_disabled_shortcuts() {
        let config = parse_config(&format!(
            "{MINIMAL_CAMERA}\n\
             [ArgusCapture.Shortcuts]\n\
             quit = <Control>q\n\
             camera-connect = none\n\
             album-videos = F5\n"
        ))
        .unwrap();

        assert_eq!(config.shortcuts().accel("quit"), "<Control>q");
        assert_eq!(config.shortcuts().accel("camera-connect"), "");
        assert_eq!(config.shortcuts().accel("album-videos"), "F5");
        // Untouched actions keep their defaults.
        assert_eq!(config.shortcuts().accel("camera-capture"), "p");
    }

    #[test]
    fn shortcuts_survive_a_render_and_parse_round_trip() {
        let mut shortcuts = Shortcuts::default();
        shortcuts.set("quit", "<Control>q");
        shortcuts.set("camera-focus", "");
        shortcuts.set("album-pictures", "<Control><Shift>p");

        let rendered = render_user_config(
            Path::new("/var/lib/argus/workspace"),
            StorageMode::WorkspaceOnly,
            &ConfiguredCamera {
                name: "CanonR3".to_owned(),
                host: "192.168.1.23".to_owned(),
                port: 80,
                username: None,
                password: None,
            },
            &shortcuts,
        );

        assert_eq!(parse_config(&rendered).unwrap().shortcuts(), &shortcuts);
    }

    #[test]
    fn formats_accelerators_for_display() {
        assert_eq!(display_accelerator("q"), "Q");
        assert_eq!(display_accelerator("<Control>f"), "Ctrl+F");
        assert_eq!(display_accelerator("<Shift><Control>p"), "Ctrl+Shift+P");
        assert_eq!(display_accelerator("F5"), "F5");
        assert_eq!(display_accelerator(""), "Not set");
    }

    #[test]
    fn normalizes_equivalent_accelerators() {
        assert_eq!(
            normalize_accelerator("<Primary>F"),
            normalize_accelerator("<Control>f")
        );
        assert_eq!(
            normalize_accelerator("<Shift><Control>p"),
            normalize_accelerator("<Control><Shift>P")
        );
        assert_ne!(
            normalize_accelerator("f"),
            normalize_accelerator("<Control>f")
        );
        assert_eq!(normalize_accelerator(""), "");
    }

    #[test]
    fn detects_conflicting_shortcuts() {
        let mut shortcuts = Shortcuts::default();
        assert!(shortcuts.conflict().is_none());

        shortcuts.set("album-pictures", "<Primary>Q");
        shortcuts.set("quit", "<Control>q");
        let (first, second) = shortcuts.conflict().unwrap();
        assert_eq!(first.action, "quit");
        assert_eq!(second.action, "album-pictures");

        // Two actions with no shortcut at all are not a conflict.
        shortcuts.set("album-pictures", "");
        shortcuts.set("quit", "");
        assert!(shortcuts.conflict().is_none());
    }

    #[test]
    fn shortcut_titles_follow_the_menu_path() {
        let connect = SHORTCUT_DEFS
            .iter()
            .find(|def| def.action == "camera-connect")
            .unwrap();

        assert_eq!(connect.title(), "Camera > Connect");
    }

    #[test]
    fn finds_menu_items_by_menu_name_label_or_shortcut() {
        let find_of = |query: &str| -> Vec<&'static str> {
            let shortcuts = Shortcuts::default();
            SHORTCUT_DEFS
                .iter()
                .filter(|def| shortcut_matches(def, shortcuts.accel(def.action), query))
                .map(|def| def.action)
                .collect()
        };

        assert_eq!(find_of("").len(), SHORTCUT_DEFS.len());
        assert_eq!(
            find_of("camera"),
            vec![
                "camera-connect",
                "camera-disconnect",
                "camera-capture",
                "camera-focus"
            ]
        );
        assert_eq!(find_of("CONFIGURATION"), vec!["edit-configuration"]);
        assert_eq!(find_of("file"), vec!["quit"]);
        assert_eq!(find_of("edit not set"), vec!["edit-configuration"]);
        assert!(find_of("no such item").is_empty());
    }
}
