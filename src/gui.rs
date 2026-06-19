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
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
    Mutex,
};
use std::thread;
use std::time::Duration;

use gdk_pixbuf::PixbufLoader;
use gtk::gio;
use gtk::glib::{self, ControlFlow, SourceId};
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, Dialog, Entry, Grid, Label,
    Orientation, Picture, PopoverMenuBar, ResponseType, SpinButton, Stack,
};
use tokio::runtime::Builder;

use crate::config::{self, AppConfig, ConfiguredCamera};

const APP_ID: &str = "org.arguscapture.ArgusCapture";
const APP_NAME: &str = "Argus Capture";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LIVE_VIEW_STREAM: &str = "/brapi/shooting/lvscrolldetail?liveviewsize=medium";

struct LiveViewSession {
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
    session_cookie: Arc<Mutex<Option<String>>>,
    ui_source: SourceId,
    worker: thread::JoinHandle<()>,
}

enum LiveViewEvent {
    Frame(Vec<u8>),
    Error(String),
}

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
    let rendered_frame_count = Rc::new(Cell::new(0_u64));
    let live_view_session: Rc<RefCell<Option<LiveViewSession>>> = Rc::new(RefCell::new(None));
    let connect_action = gio::SimpleAction::new("camera-connect", None);
    let disconnect_action = gio::SimpleAction::new("camera-disconnect", None);
    let focus_action = gio::SimpleAction::new("camera-focus", None);
    let configuration_action = gio::SimpleAction::new("edit-configuration", None);
    let about_action = gio::SimpleAction::new("help-about", None);
    let quit_action = gio::SimpleAction::new("quit", None);

    application.add_action(&connect_action);
    application.add_action(&disconnect_action);
    application.add_action(&focus_action);
    application.add_action(&configuration_action);
    application.add_action(&about_action);
    application.add_action(&quit_action);

    application.set_accels_for_action("app.quit", &["q"]);
    application.set_accels_for_action("app.camera-connect", &["c"]);
    application.set_accels_for_action("app.camera-disconnect", &["d"]);
    application.set_accels_for_action("app.camera-focus", &["f"]);
    application.set_accels_for_action("app.help-about", &["a"]);

    let (content, content_stack, live_view_picture) = build_content_view();
    update_connection_state(
        false,
        &status_label,
        &connect_action,
        &disconnect_action,
        &focus_action,
        &content_stack,
    );

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
        let focus_action = focus_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let focus_action_state = focus_action.clone();
        let content_stack = content_stack.clone();
        let live_view_picture = live_view_picture.clone();
        let live_view_session = live_view_session.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let connected = connected.clone();
        let configured_camera = configured_camera.clone();
        connect_action.clone().connect_activate(move |_, _| {
            let configured_camera = configured_camera.borrow().clone();
            if configured_camera.host.trim().is_empty() {
                status_label.set_text("No camera configured.");
                return;
            }

            log_live_view(format!(
                "connect requested for {}:{} ({})",
                configured_camera.host, configured_camera.port, configured_camera.name
            ));
            connected.set(true);
            update_connection_state(
                true,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
                &focus_action_state,
                &content_stack,
            );
            status_label.set_text("Connecting to camera...");

            if let Some(session) = live_view_session.borrow_mut().take() {
                session.stop.store(true, Ordering::Relaxed);
                session.ui_source.remove();
                let _ = session.worker.join();
            }

            rendered_frame_count.set(0);
            let rendered_frame_counter = rendered_frame_count.clone();
            let session = start_live_view_session(
                configured_camera,
                live_view_picture.clone(),
                status_label.clone(),
                rendered_frame_counter,
            );
            *live_view_session.borrow_mut() = Some(session);
        });
    }

    {
        let status_label = status_label.clone();
        let connect_action = connect_action.clone();
        let disconnect_action = disconnect_action.clone();
        let focus_action = focus_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let focus_action_state = focus_action.clone();
        let content_stack = content_stack.clone();
        let live_view_picture = live_view_picture.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let connected = connected.clone();
        disconnect_action.clone().connect_activate(move |_, _| {
            log_live_view("disconnect requested");
            connected.set(false);
            if let Some(session) = live_view_session.borrow_mut().take() {
                session.stop.store(true, Ordering::Relaxed);
                if let Ok(pid_slot) = session.child_pid.lock()
                    && let Some(pid) = *pid_slot
                {
                    let pid_string = pid.to_string();
                    if let Ok(status) = Command::new("kill").arg("-0").arg(&pid_string).status()
                        && status.success()
                    {
                        log_live_view(format!("killing live-view curl process {pid}"));
                        let _ = Command::new("kill").arg(pid_string).status();
                    }
                }
                session.ui_source.remove();
                let _ = session.worker.join();
            }
            let camera = configured_camera.borrow().clone();
            if !camera.host.trim().is_empty() {
                let _ = run_curl_request(
                    "GET",
                    &format!("http://{}:{}/brapi/logout", camera.host, camera.port),
                    None,
                    None,
                    None,
                );
            }
            rendered_frame_count.set(0);
            live_view_picture.set_paintable(Option::<&gtk::gdk::Texture>::None);
            update_connection_state(
                false,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
                &focus_action_state,
                &content_stack,
            );
        });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        focus_action.connect_activate(move |_, _| {
            let camera = configured_camera.borrow().clone();
            let cookie = live_view_session
                .borrow()
                .as_ref()
                .and_then(|session| session.session_cookie.lock().ok()?.clone());

            let Some(cookie) = cookie else {
                status_label.set_text("Focus unavailable: no active camera session.");
                return;
            };

            log_live_view(format!("focus requested for {}:{}", camera.host, camera.port));
            status_label.set_text("Focusing...");
            match trigger_focus(&camera, &cookie) {
                Ok(()) => status_label.set_text("Focus complete."),
                Err(error) => {
                    log_live_view(format!("focus failed: {error}"));
                    status_label.set_text(&format!("Focus error: {error}"));
                }
            }
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
    camera_menu.append(Some("Focus"), Some("app.camera-focus"));
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
    let focus_button = Button::with_label("Focus");
    focus_button.set_action_name(Some("app.camera-focus"));

    toolbar.append(&connect_button);
    toolbar.append(&disconnect_button);
    toolbar.append(&focus_button);
    toolbar
}

fn build_content_view() -> (GtkBox, Stack, Picture) {
    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let disconnected = GtkBox::new(Orientation::Vertical, 12);
    disconnected.set_hexpand(true);
    disconnected.set_vexpand(true);
    disconnected.set_halign(Align::Center);
    disconnected.set_valign(Align::Center);

    let logo = Picture::for_filename(logo_path());
    logo.set_halign(Align::Center);
    logo.set_valign(Align::Center);
    disconnected.append(&logo);

    let connected = GtkBox::new(Orientation::Vertical, 12);
    connected.set_hexpand(true);
    connected.set_vexpand(true);
    let live_view_picture = Picture::new();
    live_view_picture.set_halign(Align::Fill);
    live_view_picture.set_valign(Align::Fill);
    live_view_picture.set_hexpand(true);
    live_view_picture.set_vexpand(true);
    live_view_picture.set_can_shrink(true);
    live_view_picture.set_keep_aspect_ratio(true);
    connected.append(&live_view_picture);

    stack.add_named(&disconnected, Some("disconnected"));
    stack.add_named(&connected, Some("connected"));
    content.append(&stack);
    (content, stack, live_view_picture)
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
    focus_action: &gio::SimpleAction,
    content_stack: &Stack,
) {
    status_label.set_text(if connected {
        "Camera connected."
    } else {
        "Camera disconnected."
    });
    connect_action.set_enabled(!connected);
    disconnect_action.set_enabled(connected);
    focus_action.set_enabled(connected);
    content_stack.set_visible_child_name(if connected {
        "connected"
    } else {
        "disconnected"
    });
}

fn logo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("doc/logo/logo-256x256.png")
}

fn start_live_view_session(
    configured_camera: ConfiguredCamera,
    live_view_picture: Picture,
    status_label: Label,
    rendered_frame_count: Rc<Cell<u64>>,
) -> LiveViewSession {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = stop.clone();
    let stop_ui = stop.clone();
    let child_pid = Arc::new(Mutex::new(None));
    let child_pid_worker = child_pid.clone();
    let session_cookie = Arc::new(Mutex::new(None));
    let session_cookie_worker = session_cookie.clone();
    let (sender, receiver) = mpsc::channel::<LiveViewEvent>();

    let ui_source = glib::timeout_add_local(Duration::from_millis(33), move || {
        while let Ok(event) = receiver.try_recv() {
            match event {
                LiveViewEvent::Frame(frame) => {
                    if let Err(error) = update_picture_from_frame(&live_view_picture, &frame) {
                        log_live_view(format!("frame decode failed: {error}"));
                        status_label.set_text(&format!("Live view decode error: {error}"));
                    } else {
                        let rendered = rendered_frame_count.get() + 1;
                        rendered_frame_count.set(rendered);
                        if rendered <= 5 || rendered.is_multiple_of(30) {
                            log_live_view(format!(
                                "rendered frame #{rendered} ({} bytes)",
                                frame.len()
                            ));
                        }
                        status_label.set_text("Live view active.");
                    }
                }
                LiveViewEvent::Error(error) => {
                    log_live_view(format!("session error: {error}"));
                    status_label.set_text(&format!("Live view error: {error}"));
                }
            }
        }

        if stop_ui.load(Ordering::Relaxed) {
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });

    let worker = thread::spawn(move || {
        log_live_view(format!(
            "starting live-view worker for {}:{}",
            configured_camera.host, configured_camera.port
        ));
        let runtime = Builder::new_current_thread().enable_all().build();
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = sender.send(LiveViewEvent::Error(error.to_string()));
                return;
            }
        };

        if let Err(error) = runtime.block_on(run_live_view_session(
            configured_camera,
            sender.clone(),
            stop_worker.clone(),
            child_pid_worker,
            session_cookie_worker,
        )) && !stop_worker.load(Ordering::Relaxed)
        {
            let _ = sender.send(LiveViewEvent::Error(error));
        }
    });

    LiveViewSession {
        stop,
        child_pid,
        session_cookie,
        ui_source,
        worker,
    }
}

async fn run_live_view_session(
    configured_camera: ConfiguredCamera,
    sender: mpsc::Sender<LiveViewEvent>,
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
    session_cookie_slot: Arc<Mutex<Option<String>>>,
) -> Result<(), String> {
    let base_url = format!(
        "http://{}:{}",
        configured_camera.host, configured_camera.port
    );
    log_live_view(format!("live-view session base URL: {base_url}"));
    let _ = run_curl_request("GET", &format!("{base_url}/brapi/logout"), None, None, None);

    let session_cookie = login_browser_remote(&base_url, &configured_camera)?;
    if let Ok(mut slot) = session_cookie_slot.lock() {
        *slot = Some(session_cookie.clone());
    }
    log_live_view(format!(
        "browser remote login succeeded; cookie prefix={}",
        session_cookie.split('=').next().unwrap_or_default()
    ));

    prepare_browser_remote_shooting_page(&base_url, &session_cookie)?;
    stream_live_view(&base_url, &session_cookie, sender, stop.clone(), child_pid)?;

    if !stop.load(Ordering::Relaxed) {
        let _ = run_curl_request("GET", &format!("{base_url}/brapi/logout"), None, None, None);
    }

    if let Ok(mut slot) = session_cookie_slot.lock() {
        *slot = None;
    }

    Ok(())
}

fn login_browser_remote(
    base_url: &str,
    configured_camera: &ConfiguredCamera,
) -> Result<String, String> {
    let (username, password) = configured_camera
        .credentials()
        .ok_or_else(|| "Browser Remote requires username and password".to_owned())?;
    let output = Command::new("curl")
        .args([
            "-sS",
            "--digest",
            "-u",
            &format!("{username}:{password}"),
            "-D",
            "-",
            "-o",
            "/dev/null",
            &format!("{base_url}/brapi/login"),
        ])
        .output()
        .map_err(|error| format!("failed to run curl for Browser Remote login: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl Browser Remote login failed: {stderr}"));
    }

    let headers = String::from_utf8_lossy(&output.stdout);
    log_live_view(format!(
        "browser remote login headers:\n{}",
        headers.trim_end()
    ));
    let location = headers
        .lines()
        .filter_map(|line| line.strip_prefix("Location:"))
        .map(str::trim)
        .next_back()
        .unwrap_or_default();

    if location == "/wpd/already_login.shtml" {
        return Err("Browser Remote is already in use".to_owned());
    }

    if location != "/wpd/topmenu.shtml" {
        return Err(format!("unexpected Browser Remote landing page `{location}`"));
    }

    headers
        .lines()
        .filter_map(|line| line.strip_prefix("Set-Cookie:"))
        .map(str::trim)
        .filter_map(|line| line.split(';').next())
        .find(|cookie| cookie.starts_with("brsessionid="))
        .map(str::to_owned)
        .ok_or_else(|| "missing Browser Remote session cookie".to_owned())
}

fn prepare_browser_remote_shooting_page(base_url: &str, session_cookie: &str) -> Result<(), String> {
    let (status, body) = run_curl_request(
        "GET",
        &format!("{base_url}/wpd/shoot.shtml"),
        Some(session_cookie),
        Some(&format!("{base_url}/wpd/topmenu.shtml")),
        None,
    )?;
    log_live_view(format!(
        "shoot page response status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status != 200 {
        return Err(format!("shoot page load failed with {status}: {body}"));
    }

    let (status, body) = run_curl_request(
        "GET",
        &format!("{base_url}/brapi/currentproperty"),
        Some(session_cookie),
        Some(&format!("{base_url}/wpd/shoot.shtml")),
        None,
    )?;
    log_live_view(format!(
        "currentproperty response status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!("currentproperty request failed with {status}: {body}"))
    }
}

fn stream_live_view(
    base_url: &str,
    session_cookie: &str,
    sender: mpsc::Sender<LiveViewEvent>,
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
) -> Result<(), String> {
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let header_path = std::env::temp_dir().join(format!("argus-capture-liveview-{}.headers", std::process::id()));
    log_live_view(format!(
        "opening live-view stream {}{}",
        base_url, LIVE_VIEW_STREAM
    ));
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--no-buffer",
            "-H",
            &format!("Cookie: {session_cookie}"),
            "-e",
            &referer,
            "-D",
        ])
        .arg(&header_path)
        .arg(format!("{base_url}{LIVE_VIEW_STREAM}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start live view stream: {error}"))?;
    if let Ok(mut pid_slot) = child_pid.lock() {
        *pid_slot = Some(child.id());
    }
    log_live_view(format!("spawned curl live-view process pid={}", child.id()));

    let mut status = None;
    for _ in 0..20 {
        if let Ok(contents) = fs::read_to_string(&header_path)
            && let Some(found) = contents
                .lines()
                .filter_map(|line| line.strip_prefix("HTTP/1.1 "))
                .filter_map(|line| line.split_whitespace().next())
                .filter_map(|code| code.parse::<u16>().ok())
                .next_back()
        {
            status = Some(found);
            log_live_view(format!(
                "live-view stream headers observed status={} headers={}",
                found,
                contents.trim_end()
            ));
            break;
        }

        if child.try_wait().map_err(|error| error.to_string())?.is_some() {
            break;
        }

        thread::sleep(Duration::from_millis(50));
    }

    if status == Some(503) {
        let output = child.wait_with_output().map_err(|error| error.to_string())?;
        if let Ok(mut pid_slot) = child_pid.lock() {
            *pid_slot = None;
        }
        let body = String::from_utf8_lossy(&output.stdout).to_string();
        log_live_view(format!(
            "live-view stream returned 503 body_prefix={}",
            preview_text(&body)
        ));
        return Err(format!("live view stream failed with 503: {body}"));
    }

    if status != Some(200) {
        let output = child.wait_with_output().map_err(|error| error.to_string())?;
        if let Ok(mut pid_slot) = child_pid.lock() {
            *pid_slot = None;
        }
        let body = String::from_utf8_lossy(&output.stdout).to_string();
        log_live_view(format!(
            "live-view stream failed before frame loop status={} body_prefix={}",
            status.unwrap_or_default(),
            preview_text(&body)
        ));
        return Err(format!(
            "live view stream failed with {}: {body}",
            status.unwrap_or_default()
        ));
    }

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing live view stdout stream".to_owned())?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut parsed_frames = 0_u64;

    while !stop.load(Ordering::Relaxed) {
        let read = stdout.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            log_live_view("live-view stream reached EOF");
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        drain_live_view_frames(&mut buffer, &sender, &mut parsed_frames);
    }

    let _ = child.kill();
    let _ = child.wait();
    if let Ok(mut pid_slot) = child_pid.lock() {
        *pid_slot = None;
    }
    let _ = fs::remove_file(header_path);
    Ok(())
}

fn run_curl_request(
    method: &str,
    url: &str,
    cookie: Option<&str>,
    referer: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let mut command = Command::new("curl");
    command.args(["-sS", "-X", method]);

    if let Some(cookie) = cookie {
        command.args(["-H", &format!("Cookie: {cookie}")]);
    }
    if let Some(referer) = referer {
        command.args(["-e", referer]);
    }
    if let Some(body) = body {
        command.args([
            "-H",
            "Content-Type: application/json; charset=utf-8",
            "-H",
            "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT",
            "-d",
            body,
        ]);
    }

    command.args(["-w", "\n__STATUS__:%{http_code}", url]);
    let output = command
        .output()
        .map_err(|error| format!("failed to run curl request: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let marker = "\n__STATUS__:";
    let (body, status) = stdout
        .rsplit_once(marker)
        .ok_or_else(|| "curl response missing status marker".to_owned())?;
    let status = status.trim().parse::<u16>().map_err(|error| error.to_string())?;

    if !output.status.success() && status == 0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl request failed: {stderr}"));
    }

    Ok((status, body.to_owned()))
}

fn trigger_focus(camera: &ConfiguredCamera, session_cookie: &str) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");

    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/af"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"action":"start"}"#),
    )?;
    log_live_view(format!(
        "focus start status={status} body_prefix={}",
        preview_text(&body)
    ));
    if status != 200 {
        return Err(format!("focus start failed with {status}: {body}"));
    }

    thread::sleep(Duration::from_millis(350));

    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/af"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"action":"stop"}"#),
    )?;
    log_live_view(format!(
        "focus stop status={status} body_prefix={}",
        preview_text(&body)
    ));
    if status == 200 {
        Ok(())
    } else {
        Err(format!("focus stop failed with {status}: {body}"))
    }
}

fn drain_live_view_frames(
    buffer: &mut Vec<u8>,
    sender: &mpsc::Sender<LiveViewEvent>,
    parsed_frames: &mut u64,
) {
    let mut cursor = 0usize;

    while cursor + 9 <= buffer.len() {
        if buffer[cursor] != 0xFF || buffer[cursor + 1] != 0x00 {
            cursor += 1;
            continue;
        }

        let payload_size = u32::from_be_bytes([
            buffer[cursor + 3],
            buffer[cursor + 4],
            buffer[cursor + 5],
            buffer[cursor + 6],
        ]) as usize;
        let payload_start = cursor + 7;
        let payload_end = payload_start + payload_size;
        let frame_end = payload_end + 2;

        if frame_end > buffer.len() {
            break;
        }

        if buffer[payload_end] != 0xFF || buffer[payload_end + 1] != 0xFF {
            cursor += 1;
            continue;
        }

        let payload = &buffer[payload_start..payload_end];
        *parsed_frames += 1;
        if *parsed_frames <= 5 || (*parsed_frames).is_multiple_of(30) {
            log_live_view(format!(
                "parsed frame #{} type={} payload_size={} jpeg={} eoi={}",
                *parsed_frames,
                buffer[cursor + 2],
                payload.len(),
                payload.starts_with(&[0xFF, 0xD8]),
                payload.ends_with(&[0xFF, 0xD9]),
            ));
        }
        if payload.starts_with(&[0xFF, 0xD8]) {
            let _ = sender.send(LiveViewEvent::Frame(payload.to_vec()));
        }
        cursor = frame_end;
    }

    if cursor > 0 {
        buffer.drain(..cursor);
    }
}

fn update_picture_from_frame(picture: &Picture, frame: &[u8]) -> Result<(), glib::Error> {
    let loader = PixbufLoader::with_type("jpeg")?;
    loader.write(frame)?;
    loader.close()?;

    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| glib::Error::new(glib::FileError::Failed, "missing decoded pixbuf"))?;
    log_live_view(format!(
        "decoded JPEG into pixbuf {}x{}",
        pixbuf.width(),
        pixbuf.height()
    ));
    picture.set_pixbuf(Some(&pixbuf));
    Ok(())
}

fn log_live_view(message: impl AsRef<str>) {
    eprintln!("[argus-capture liveview] {}", message.as_ref());
}

fn preview_text(text: &str) -> String {
    let single_line = text.replace('\n', "\\n");
    if single_line.len() > 160 {
        format!("{}...", &single_line[..160])
    } else {
        single_line
    }
}
