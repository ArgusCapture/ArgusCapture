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

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, Dialog, Entry, Grid, Label,
    Orientation, Picture, PopoverMenuBar, ResponseType, SpinButton,
};

use crate::config::{self, AppConfig, ConfiguredCamera};

const APP_ID: &str = "org.arguscapture.ArgusCapture";
const APP_NAME: &str = "Argus Capture";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn run(config: Option<&AppConfig>) {
    let application = Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    let configured_camera = Rc::new(RefCell::new(initial_camera_config(config)));

    application.connect_activate(move |application| {
        build_ui(application, configured_camera.clone());
    });

    let _ = application.run();
}

fn build_ui(application: &Application, configured_camera: Rc<RefCell<ConfiguredCamera>>) {
    let window = ApplicationWindow::builder()
        .application(application)
        .title("Argus Capture")
        .default_width(960)
        .default_height(640)
        .build();

    let status_label = Label::new(Some("Camera disconnected."));
    status_label.set_halign(Align::Start);
    status_label.set_margin_top(12);
    status_label.set_margin_bottom(12);
    status_label.set_margin_start(12);
    status_label.set_margin_end(12);

    let connected = Rc::new(Cell::new(false));
    let connect_action = gio::SimpleAction::new("camera-connect", None);
    let disconnect_action = gio::SimpleAction::new("camera-disconnect", None);
    let configuration_action = gio::SimpleAction::new("edit-configuration", None);
    let about_action = gio::SimpleAction::new("help-about", None);
    let quit_action = gio::SimpleAction::new("quit", None);

    application.add_action(&connect_action);
    application.add_action(&disconnect_action);
    application.add_action(&configuration_action);
    application.add_action(&about_action);
    application.add_action(&quit_action);

    application.set_accels_for_action("app.quit", &["q"]);
    application.set_accels_for_action("app.camera-connect", &["c"]);
    application.set_accels_for_action("app.camera-disconnect", &["d"]);
    application.set_accels_for_action("app.help-about", &["a"]);

    update_connection_state(false, &status_label, &connect_action, &disconnect_action);

    {
        let application = application.clone();
        quit_action.connect_activate(move |_, _| {
            application.quit();
        });
    }

    {
        let status_label = status_label.clone();
        let connect_action = connect_action.clone();
        let disconnect_action = disconnect_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let connected = connected.clone();
        let configured_camera = configured_camera.clone();
        connect_action.clone().connect_activate(move |_, _| {
            let configured_camera = configured_camera.borrow();
            if configured_camera.host.trim().is_empty() {
                status_label.set_text("No camera configured.");
                return;
            }

            connected.set(true);
            update_connection_state(
                true,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
            );
        });
    }

    {
        let status_label = status_label.clone();
        let connect_action = connect_action.clone();
        let disconnect_action = disconnect_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let connected = connected.clone();
        disconnect_action.clone().connect_activate(move |_, _| {
            connected.set(false);
            update_connection_state(
                false,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
            );
        });
    }

    {
        let configured_camera = configured_camera.clone();
        let status_label = status_label.clone();
        let window = window.clone();
        configuration_action.connect_activate(move |_, _| {
            present_configuration_dialog(&window, configured_camera.clone(), &status_label);
        });
    }

    {
        let window = window.clone();
        about_action.connect_activate(move |_, _| {
            present_about_dialog(&window);
        });
    }

    let menu_bar = build_menu_bar_row();
    let toolbar = build_toolbar();
    let content = build_content_placeholder();

    let root = GtkBox::new(Orientation::Vertical, 0);
    root.append(&menu_bar);
    root.append(&toolbar);
    root.append(&content);
    root.append(&status_label);

    window.set_child(Some(&root));
    window.present();
}

fn build_menu_bar_row() -> GtkBox {
    let left_root = gio::Menu::new();

    let file_menu = gio::Menu::new();
    file_menu.append(Some("Quit"), Some("app.quit"));
    left_root.append_submenu(Some("File"), &file_menu);

    let edit_menu = gio::Menu::new();
    edit_menu.append(Some("Configuration"), Some("app.edit-configuration"));
    left_root.append_submenu(Some("Edit"), &edit_menu);

    let camera_menu = gio::Menu::new();
    camera_menu.append(Some("Connect"), Some("app.camera-connect"));
    camera_menu.append(Some("Disconnect"), Some("app.camera-disconnect"));
    left_root.append_submenu(Some("Camera"), &camera_menu);

    let menu_row = GtkBox::new(Orientation::Horizontal, 0);
    let left_menu_bar = PopoverMenuBar::from_model(Some(&left_root));
    left_menu_bar.set_hexpand(false);

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    let right_root = gio::Menu::new();
    let help_menu = gio::Menu::new();
    help_menu.append(Some("About"), Some("app.help-about"));
    right_root.append_submenu(Some("Help"), &help_menu);

    let right_menu_bar = PopoverMenuBar::from_model(Some(&right_root));
    right_menu_bar.set_hexpand(false);

    menu_row.append(&left_menu_bar);
    menu_row.append(&spacer);
    menu_row.append(&right_menu_bar);
    menu_row
}

fn build_toolbar() -> GtkBox {
    let toolbar = GtkBox::new(Orientation::Horizontal, 6);
    toolbar.set_margin_top(6);
    toolbar.set_margin_bottom(6);
    toolbar.set_margin_start(6);
    toolbar.set_margin_end(6);

    let connect_button = Button::with_label("Connect");
    connect_button.set_action_name(Some("app.camera-connect"));
    let disconnect_button = Button::with_label("Disconnect");
    disconnect_button.set_action_name(Some("app.camera-disconnect"));

    toolbar.append(&connect_button);
    toolbar.append(&disconnect_button);
    toolbar
}

fn build_content_placeholder() -> GtkBox {
    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let placeholder = Label::new(Some("Live view and camera controls will appear here."));
    placeholder.set_halign(Align::Center);
    placeholder.set_valign(Align::Center);
    placeholder.set_hexpand(true);
    placeholder.set_vexpand(true);

    content.append(&placeholder);
    content
}

fn present_configuration_dialog(
    parent: &ApplicationWindow,
    configured_camera: Rc<RefCell<ConfiguredCamera>>,
    status_label: &Label,
) {
    let current = configured_camera.borrow().clone();

    let dialog = Dialog::builder()
        .title("Configuration")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Save", ResponseType::Accept);

    let content_area = dialog.content_area();
    content_area.set_spacing(12);
    content_area.set_margin_top(12);
    content_area.set_margin_bottom(12);
    content_area.set_margin_start(12);
    content_area.set_margin_end(12);

    let grid = Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .hexpand(true)
        .build();

    let camera_name_entry = Entry::builder().text(&current.name).hexpand(true).build();
    let host_entry = Entry::builder().text(&current.host).hexpand(true).build();
    let port_spin = SpinButton::with_range(1.0, 65535.0, 1.0);
    port_spin.set_value(current.port as f64);
    let username_entry = Entry::builder()
        .text(current.username.as_deref().unwrap_or_default())
        .hexpand(true)
        .build();
    let password_entry = Entry::builder()
        .text(current.password.as_deref().unwrap_or_default())
        .hexpand(true)
        .visibility(false)
        .build();

    attach_form_row(&grid, 0, "Camera", &camera_name_entry);
    attach_form_row(&grid, 1, "Host", &host_entry);
    attach_form_row(&grid, 2, "Port", &port_spin);
    attach_form_row(&grid, 3, "Username", &username_entry);
    attach_form_row(&grid, 4, "Password", &password_entry);

    content_area.append(&grid);

    let configured_camera_state = configured_camera.clone();
    let status_label = status_label.clone();
    dialog.connect_response(move |dialog, response| {
        if response != ResponseType::Accept {
            dialog.close();
            return;
        }

        let camera_name = camera_name_entry.text().trim().to_owned();
        let host = host_entry.text().trim().to_owned();
        let username = optional_entry_text(&username_entry);
        let password = optional_entry_text(&password_entry);

        if camera_name.is_empty() {
            status_label.set_text("Configuration requires a camera name.");
            return;
        }

        if host.is_empty() {
            status_label.set_text("Configuration requires a host.");
            return;
        }

        if username.is_some() != password.is_some() {
            status_label.set_text("Configuration requires both username and password or neither.");
            return;
        }

        let camera = ConfiguredCamera {
            name: camera_name,
            host,
            port: port_spin.value_as_int() as u16,
            username,
            password,
        };

        match save_configuration(&camera) {
            Ok(()) => {
                *configured_camera_state.borrow_mut() = camera;
                status_label.set_text("Configuration saved.");
                dialog.close();
            }
            Err(error) => {
                status_label.set_text(&format!("Failed to save configuration: {error}"));
            }
        }
    });

    dialog.present();
}

fn present_about_dialog(parent: &ApplicationWindow) {
    let dialog = Dialog::builder()
        .title("About")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();
    dialog.add_button("Close", ResponseType::Close);

    let content_area = dialog.content_area();
    content_area.set_spacing(16);
    content_area.set_margin_top(16);
    content_area.set_margin_bottom(16);
    content_area.set_margin_start(24);
    content_area.set_margin_end(24);

    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_halign(Align::Center);
    content.set_valign(Align::Center);

    let logo = Picture::for_filename(logo_path());
    logo.set_halign(Align::Center);

    let name_label = Label::new(Some(APP_NAME));
    name_label.set_halign(Align::Center);
    name_label.add_css_class("title-2");

    let version_label = Label::new(Some(APP_VERSION));
    version_label.set_halign(Align::Center);

    content.append(&logo);
    content.append(&name_label);
    content.append(&version_label);
    content_area.append(&content);

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
    dialog.present();
}

fn attach_form_row<W: IsA<gtk::Widget>>(grid: &Grid, row: i32, label: &str, widget: &W) {
    let label = Label::builder().label(label).halign(Align::End).build();
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

fn optional_entry_text(entry: &Entry) -> Option<String> {
    let text = entry.text();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn save_configuration(camera: &ConfiguredCamera) -> io::Result<()> {
    let path = config::user_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    config::write_user_config(&path, camera)
}

fn initial_camera_config(config: Option<&AppConfig>) -> ConfiguredCamera {
    config
        .map(|app_config| app_config.selected_camera().clone())
        .unwrap_or_else(|| ConfiguredCamera {
            name: "Camera1".to_owned(),
            host: String::new(),
            port: 80,
            username: None,
            password: None,
        })
}

fn update_connection_state(
    connected: bool,
    status_label: &Label,
    connect_action: &gio::SimpleAction,
    disconnect_action: &gio::SimpleAction,
) {
    status_label.set_text(if connected {
        "Camera connected."
    } else {
        "Camera disconnected."
    });
    connect_action.set_enabled(!connected);
    disconnect_action.set_enabled(connected);
}

fn logo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("doc/logo/logo-256x256.png")
}
