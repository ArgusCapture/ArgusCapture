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

use std::error::Error;
use std::fs;
use std::io::{self, Write};

use rpassword::prompt_password as read_password;

use crate::config::{self, ConfiguredCamera};
use crate::network::{self, NetworkCamera};

type InitResult<T> = Result<T, Box<dyn Error>>;

const DEFAULT_CAMERA_NAME: &str = "Camera1";

pub(crate) async fn initialize_user_config() -> InitResult<()> {
    let config_path = config::user_config_path()?;

    if config_path.exists()
        && !prompt_yes_no(
            &format!(
                "Configuration already exists at {}. Overwrite?",
                config_path.display()
            ),
            false,
        )?
    {
        println!("Initialization cancelled.");
        return Ok(());
    }

    let cameras = network::discover_network_cameras(None).await;
    let selected_camera = choose_camera(&cameras);
    let default_workspace = config::default_workspace();
    let storage = config::default_storage();

    let mut configured_camera = ConfiguredCamera {
        name: suggest_camera_name(selected_camera.as_ref()),
        host: selected_camera
            .as_ref()
            .map(|camera| camera.address.clone())
            .unwrap_or_default(),
        port: selected_camera.as_ref().map_or(80, |camera| camera.port),
        username: None,
        password: None,
    };
    let workspace = prompt_with_default("Workspace", &default_workspace.display().to_string())?;

    if configured_camera.host.is_empty() {
        configured_camera.host = prompt_required("Host")?;
    } else {
        configured_camera.host = prompt_with_default("Host", &configured_camera.host)?;
    }
    configured_camera.port = prompt_port(configured_camera.port)?;

    configured_camera.username = prompt_optional("Username")?;
    configured_camera.password = match configured_camera.username.as_deref() {
        Some(_) => Some(prompt_password("Password")?),
        None => None,
    };

    if let Some(camera) = network::inspect_configured_camera(&configured_camera).await {
        configured_camera.name = suggest_camera_name(Some(&camera));
    }

    configured_camera.name = prompt_with_default("Camera name", &configured_camera.name)?;

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }

    config::write_user_config(
        &config_path,
        std::path::Path::new(&workspace),
        storage,
        &configured_camera,
    )?;
    println!("Created {}", config_path.display());

    Ok(())
}

fn choose_camera(cameras: &[NetworkCamera]) -> Option<NetworkCamera> {
    match cameras {
        [] => {
            println!("No cameras were detected.");
            None
        }
        [camera, ..] => Some(camera.clone()),
    }
}

fn prompt_required(label: &str) -> io::Result<String> {
    loop {
        let value = prompt_line(label)?;
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }

        println!("A value is required.");
    }
}

fn prompt_optional(label: &str) -> io::Result<Option<String>> {
    let value = prompt_line(&format!("{label} []"))?;
    let trimmed = value.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

fn prompt_with_default(label: &str, default: &str) -> io::Result<String> {
    let value = prompt_line(&format!("{label} [{default}]"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn prompt_port(default: u16) -> io::Result<u16> {
    loop {
        let value = prompt_with_default("Port", &default.to_string())?;
        match value.parse::<u16>() {
            Ok(port) => return Ok(port),
            Err(_) => println!("Enter a valid port number."),
        }
    }
}

fn prompt_password(label: &str) -> io::Result<String> {
    loop {
        let pass = read_password(format!("{label}: "))?;
        if !pass.trim().is_empty() {
            return Ok(pass);
        }
        println!("Password cannot be empty when username is set.");
    }
}

fn prompt_yes_no(label: &str, default: bool) -> io::Result<bool> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };

    loop {
        let value = prompt_line(&format!("{label} {suffix}"))?;
        let trimmed = value.trim();

        if trimmed.is_empty() {
            return Ok(default);
        }

        match trimmed.to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Enter yes or no."),
        }
    }
}

fn prompt_line(label: &str) -> io::Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input)
}

fn suggest_camera_name(camera: Option<&NetworkCamera>) -> String {
    let source = camera
        .map(NetworkCamera::display_name)
        .unwrap_or_else(|| DEFAULT_CAMERA_NAME.to_owned());

    let sanitized: String = source
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .map(title_case_segment)
        .collect();
    if sanitized.is_empty() {
        DEFAULT_CAMERA_NAME.to_owned()
    } else {
        sanitized
    }
}

fn title_case_segment(segment: &str) -> String {
    let mut characters = segment.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };

    let mut value = String::new();
    value.push(first.to_ascii_uppercase());
    value.extend(characters);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_camera_name_from_display_name() {
        let camera = NetworkCamera {
            address: "192.168.1.23".to_owned(),
            port: 80,
            manufacturer: Some("Canon".to_owned()),
            model: "EOS R3".to_owned(),
        };

        assert_eq!(suggest_camera_name(Some(&camera)), "CanonEOSR3");
    }

    #[test]
    fn falls_back_to_default_camera_name() {
        assert_eq!(suggest_camera_name(None), "Camera1");
    }

    #[test]
    fn falls_back_to_model_when_manufacturer_is_missing() {
        let camera = NetworkCamera {
            address: "192.168.1.23".to_owned(),
            port: 80,
            manufacturer: None,
            model: "RLC 811A".to_owned(),
        };

        assert_eq!(suggest_camera_name(Some(&camera)), "RLC811A");
    }

    #[test]
    fn title_cases_words_in_suggested_camera_name() {
        let camera = NetworkCamera {
            address: "192.168.1.23".to_owned(),
            port: 80,
            manufacturer: Some("Canon".to_owned()),
            model: "CCAPI camera".to_owned(),
        };

        assert_eq!(suggest_camera_name(Some(&camera)), "CanonCCAPICamera");
    }

    #[test]
    fn title_cases_individual_segments() {
        assert_eq!(title_case_segment("camera"), "Camera");
        assert_eq!(title_case_segment("CCAPI"), "CCAPI");
    }
}
