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

const USER_CONFIG_DIR: &str = ".argus-capture";
const CONFIG_FILE_NAME: &str = "argus-capture.conf";
pub(crate) const DEFAULT_CONFIG_PATH: &str = "/etc/argus-capture/argus-capture.conf";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct AppConfig {
    pub(crate) path: PathBuf,
    workspace: PathBuf,
    storage: StorageMode,
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

pub(crate) fn render_user_config(
    workspace: &Path,
    storage: StorageMode,
    camera: &ConfiguredCamera,
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
) -> io::Result<()> {
    let rendered = render_user_config(workspace, storage, camera);

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
        );

        assert_eq!(
            config,
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = both\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n\
             username = abbc\n\
             password = cbbaabbc\n"
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
        );

        assert_eq!(
            config,
            "[ArgusCapture]\n\
             workspace = /var/lib/argus/workspace\n\
             storage = workspace_only\n\
             camera = CanonR3\n\n\
             [CanonR3]\n\
             host = 192.168.1.23\n\
             port = 80\n"
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
        )
        .unwrap();

        let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_file(&config_path).unwrap();
        fs::remove_dir(&temp_dir).unwrap();
    }
}
