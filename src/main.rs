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
use std::io;
use std::path::PathBuf;

use gphoto2::Context;

mod config;
mod gui;
mod init;
mod network;

type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Capture,
    Init,
    List,
    Help,
}

#[derive(Debug, Eq, PartialEq)]
struct CliOptions {
    command: Command,
    config_path: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> AppResult<()> {
    let options = parse_args()?;

    if options.command == Command::Help {
        print_usage();
        return Ok(());
    }

    if options.command == Command::Init {
        init::initialize_user_config().await?;
        return Ok(());
    }

    let config = config::AppConfig::load(options.config_path.as_deref())?;

    match options.command {
        Command::Capture => {
            gui::run(config.as_ref());
            Ok(())
        }
        Command::Init => unreachable!("init is handled before config loading"),
        Command::Help => unreachable!("help is handled before config loading"),
        Command::List => list_detected_cameras(config.as_ref()).await,
    }
}

fn parse_args() -> io::Result<CliOptions> {
    parse_args_from(std::env::args())
}

fn parse_args_from<I, S>(arguments: I) -> io::Result<CliOptions>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut arguments = arguments.into_iter().map(Into::into);
    let _program_name = arguments.next();
    let mut command = Command::Capture;
    let mut config_path = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => command = Command::Help,
            "-i" | "--init" => command = Command::Init,
            "-l" | "--list" => command = Command::List,
            "-c" | "--config" => {
                let Some(path) = arguments.next() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Missing value for `{argument}`.\n\n{}", usage()),
                    ));
                };
                config_path = Some(PathBuf::from(path));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown option `{argument}`.\n\n{}", usage()),
                ));
            }
        }
    }

    Ok(CliOptions {
        command,
        config_path,
    })
}

async fn list_detected_cameras(config: Option<&config::AppConfig>) -> AppResult<()> {
    let context = Context::new()?;
    let usb_cameras: Vec<_> = context.list_cameras().wait()?.collect();
    let network_cameras =
        network::discover_network_cameras(config.map(config::AppConfig::selected_camera)).await;

    if network_cameras.is_empty() && usb_cameras.is_empty() {
        println!(
            "No cameras detected. Connect a supported camera or ensure the ONVIF camera is \
             reachable and run again."
        );
        return Ok(());
    }

    if !network_cameras.is_empty() {
        println!("argus-capture:");
        for camera in &network_cameras {
            println!("- {} ({})", camera.display_name(), camera.address);
        }
    }

    if !usb_cameras.is_empty() {
        if !network_cameras.is_empty() {
            println!();
        }
        println!("USB/libgphoto2 cameras:");
        for camera in &usb_cameras {
            println!("- {} ({})", camera.model, camera.port);
        }
    }

    Ok(())
}

fn print_usage() {
    println!("{}", usage());
}

fn usage() -> &'static str {
    "Usage: argus-capture [OPTIONS]\n\nRunning without options launches the native GTK4 UI.\n\nOptions:\n  -c, --config FILE   load INI configuration from FILE\n  -i, --init          create ~/.argus-capture/argus-capture.conf interactively\n  -l, --list          list detected USB and ONVIF network cameras\n  -h, --help          print help\n\nIf --config is not provided, argus-capture first looks for ~/.argus-capture/argus-capture.conf and then /etc/argus-capture/argus-capture.conf."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_and_long_config_flags() {
        let options =
            parse_args_from(["argus-capture", "-l", "--config", "argus-capture.conf"]).unwrap();

        assert_eq!(
            options,
            CliOptions {
                command: Command::List,
                config_path: Some(PathBuf::from("argus-capture.conf")),
            }
        );
    }

    #[test]
    fn parses_init_flag() {
        let options = parse_args_from(["argus-capture", "--init"]).unwrap();

        assert_eq!(
            options,
            CliOptions {
                command: Command::Init,
                config_path: None,
            }
        );
    }

    #[test]
    fn defaults_to_capture_with_no_arguments() {
        let options = parse_args_from(["argus-capture"]).unwrap();

        assert_eq!(
            options,
            CliOptions {
                command: Command::Capture,
                config_path: None,
            }
        );
    }

    #[test]
    fn rejects_unknown_options() {
        let error = parse_args_from(["argus-capture", "--bogus"]).unwrap_err();

        assert!(error.to_string().contains("Unknown option `--bogus`"));
    }

    #[test]
    fn rejects_missing_config_path() {
        let error = parse_args_from(["argus-capture", "-c"]).unwrap_err();

        assert!(error.to_string().contains("Missing value for `-c`"));
    }

    #[test]
    fn rejects_missing_long_config_path() {
        let error = parse_args_from(["argus-capture", "--config"]).unwrap_err();

        assert!(error.to_string().contains("Missing value for `--config`"));
    }
}
