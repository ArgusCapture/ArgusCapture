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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use gdk_pixbuf::PixbufLoader;
use gtk::gio;
use gtk::glib::{self, ControlFlow, SourceId};
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, Dialog, DrawingArea, DropDown,
    Entry, FileChooserAction, FileChooserNative, Grid, Label, Orientation, Overlay, Picture,
    PopoverMenuBar, ResponseType, SpinButton, Stack, StackSwitcher, Switch,
};
use serde_json::Value;
use tokio::runtime::Builder;

use crate::config::{self, AppConfig, ConfiguredCamera, StorageMode};

const APP_ID: &str = "org.arguscapture.ArgusCapture";
const APP_NAME: &str = "Argus Capture";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LIVE_VIEW_STREAM: &str = "/brapi/shooting/lvscrolldetail?liveviewsize=medium";
const LOGO_16X16: &[u8] = include_bytes!("../doc/logo/logo-16x16.png");
const LOGO_32X32: &[u8] = include_bytes!("../doc/logo/logo-32x32.png");
const LOGO_64X64: &[u8] = include_bytes!("../doc/logo/logo-64x64.png");
const LOGO_128X128: &[u8] = include_bytes!("../doc/logo/logo-128x128.png");
const LOGO_256X256: &[u8] = include_bytes!("../doc/logo/logo-256x256.png");
const LOGO_512X512: &[u8] = include_bytes!("../doc/logo/logo-512x512.png");

struct LiveViewSession {
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
    session_cookie: Arc<Mutex<Option<String>>>,
    ui_source: SourceId,
    worker: thread::JoinHandle<()>,
}

enum LiveViewEvent {
    Frame(Vec<u8>),
    FocusOverlay(FocusOverlayState),
    FocusMode(FocusModeState),
    Error(String),
}

struct ConnectedView {
    content: GtkBox,
    content_stack: Stack,
    live_view_picture: Picture,
    capture_mode_switch: Switch,
    capture_button: Button,
    focus_overlay_area: DrawingArea,
    focus_operation_label: Label,
    focus_method_label: Label,
    focus_move_up_left: Button,
    focus_move_up: Button,
    focus_move_up_right: Button,
    focus_move_down_left: Button,
    focus_move_down: Button,
    focus_move_down_right: Button,
    focus_move_left: Button,
    focus_trigger_button: Button,
    focus_move_right: Button,
}

#[derive(Clone, Debug, Default)]
struct FocusOverlayState {
    image_x: f64,
    image_y: f64,
    image_width: f64,
    image_height: f64,
    frame_x: f64,
    frame_y: f64,
    frame_width: f64,
    frame_height: f64,
    active: bool,
}

#[derive(Clone, Debug, Default)]
struct FocusModeState {
    operation: String,
    method: String,
}

#[derive(Clone, Copy, Debug)]
enum FocusDirection {
    UpLeft,
    Up,
    UpRight,
    Down,
    DownLeft,
    DownRight,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureMode {
    Picture,
    Video,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturedMediaKind {
    Picture,
    Video,
}

pub(crate) fn run(config: Option<&AppConfig>) {
    let application = Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    let configured_camera = Rc::new(RefCell::new(initial_camera_config(config)));
    let workspace = Rc::new(RefCell::new(initial_workspace(config)));
    let storage = Rc::new(Cell::new(initial_storage(config)));

    application.connect_activate(move |application| {
        build_ui(
            application,
            configured_camera.clone(),
            workspace.clone(),
            storage.clone(),
        );
    });

    let _ = application.run();
}

fn build_ui(
    application: &Application,
    configured_camera: Rc<RefCell<ConfiguredCamera>>,
    workspace: Rc<RefCell<PathBuf>>,
    storage: Rc<Cell<StorageMode>>,
) {
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
    let capture_mode = Rc::new(Cell::new(CaptureMode::Picture));
    let video_recording = Rc::new(Cell::new(false));
    let rendered_frame_count = Rc::new(Cell::new(0_u64));
    let live_view_session: Rc<RefCell<Option<LiveViewSession>>> = Rc::new(RefCell::new(None));
    let focus_overlay_state = Rc::new(RefCell::new(FocusOverlayState::default()));
    let connect_action = gio::SimpleAction::new("camera-connect", None);
    let disconnect_action = gio::SimpleAction::new("camera-disconnect", None);
    let capture_action = gio::SimpleAction::new("camera-capture", None);
    let focus_action = gio::SimpleAction::new("camera-focus", None);
    let configuration_action = gio::SimpleAction::new("edit-configuration", None);
    let about_action = gio::SimpleAction::new("help-about", None);
    let quit_action = gio::SimpleAction::new("quit", None);

    application.add_action(&connect_action);
    application.add_action(&disconnect_action);
    application.add_action(&capture_action);
    application.add_action(&focus_action);
    application.add_action(&configuration_action);
    application.add_action(&about_action);
    application.add_action(&quit_action);

    application.set_accels_for_action("app.quit", &["q"]);
    application.set_accels_for_action("app.camera-connect", &["c"]);
    application.set_accels_for_action("app.camera-disconnect", &["d"]);
    application.set_accels_for_action("app.camera-capture", &["p"]);
    application.set_accels_for_action("app.camera-focus", &["f"]);
    application.set_accels_for_action("app.help-about", &["a"]);

    let connected_view = build_content_view(focus_overlay_state.clone());
    update_connection_state(
        false,
        &status_label,
        &connect_action,
        &disconnect_action,
        &capture_action,
        &focus_action,
        &connected_view.content_stack,
    );
    update_capture_mode_controls(
        false,
        capture_mode.get(),
        video_recording.get(),
        &capture_action,
        &connected_view.capture_button,
        &connected_view.capture_mode_switch,
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
        let capture_action = capture_action.clone();
        let focus_action = focus_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let capture_action_state = capture_action.clone();
        let focus_action_state = focus_action.clone();
        let content_stack = connected_view.content_stack.clone();
        let live_view_picture = connected_view.live_view_picture.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode_switch = connected_view.capture_mode_switch.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        let focus_operation_label = connected_view.focus_operation_label.clone();
        let focus_method_label = connected_view.focus_method_label.clone();
        let live_view_session = live_view_session.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let capture_mode = capture_mode.clone();
        let video_recording = video_recording.clone();
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
            video_recording.set(false);
            update_connection_state(
                true,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
                &capture_action_state,
                &focus_action_state,
                &content_stack,
            );
            update_capture_mode_controls(
                true,
                capture_mode.get(),
                video_recording.get(),
                &capture_action_state,
                &capture_button,
                &capture_mode_switch,
            );
            status_label.set_text("Connecting to camera...");

            if let Some(session) = live_view_session.borrow_mut().take() {
                session.stop.store(true, Ordering::Relaxed);
                session.ui_source.remove();
                let _ = session.worker.join();
            }

            rendered_frame_count.set(0);
            *focus_overlay_state.borrow_mut() = FocusOverlayState::default();
            focus_overlay_area.queue_draw();
            focus_operation_label.set_text("AF mode: -");
            focus_method_label.set_text("AF method: -");
            let rendered_frame_counter = rendered_frame_count.clone();
            let session = start_live_view_session(
                configured_camera,
                live_view_picture.clone(),
                status_label.clone(),
                rendered_frame_counter,
                focus_overlay_state.clone(),
                focus_overlay_area.clone(),
                focus_operation_label.clone(),
                focus_method_label.clone(),
            );
            *live_view_session.borrow_mut() = Some(session);
        });
    }

    {
        let status_label = status_label.clone();
        let connect_action = connect_action.clone();
        let disconnect_action = disconnect_action.clone();
        let capture_action = capture_action.clone();
        let focus_action = focus_action.clone();
        let connect_action_state = connect_action.clone();
        let disconnect_action_state = disconnect_action.clone();
        let capture_action_state = capture_action.clone();
        let focus_action_state = focus_action.clone();
        let content_stack = connected_view.content_stack.clone();
        let live_view_picture = connected_view.live_view_picture.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode_switch = connected_view.capture_mode_switch.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        let focus_operation_label = connected_view.focus_operation_label.clone();
        let focus_method_label = connected_view.focus_method_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let capture_mode = capture_mode.clone();
        let video_recording = video_recording.clone();
        let connected = connected.clone();
        disconnect_action.clone().connect_activate(move |_, _| {
            log_live_view("disconnect requested");
            connected.set(false);
            video_recording.set(false);
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
            *focus_overlay_state.borrow_mut() = FocusOverlayState::default();
            focus_overlay_area.queue_draw();
            focus_operation_label.set_text("AF mode: -");
            focus_method_label.set_text("AF method: -");
            update_connection_state(
                false,
                &status_label,
                &connect_action_state,
                &disconnect_action_state,
                &capture_action_state,
                &focus_action_state,
                &content_stack,
            );
            update_capture_mode_controls(
                false,
                capture_mode.get(),
                video_recording.get(),
                &capture_action_state,
                &capture_button,
                &capture_mode_switch,
            );
        });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let capture_mode = capture_mode.clone();
        let capture_action_state = capture_action.clone();
        let workspace = workspace.clone();
        let storage = storage.clone();
        let disconnect_action = disconnect_action.clone();
        let connected = connected.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode_switch = connected_view.capture_mode_switch.clone();
        let video_recording = video_recording.clone();
        capture_action.clone().connect_activate(move |_, _| {
            let camera = configured_camera.borrow().clone();
            let cookie = live_view_session
                .borrow()
                .as_ref()
                .and_then(|session| session.session_cookie.lock().ok()?.clone());

            let Some(cookie) = cookie else {
                status_label.set_text(if capture_mode.get() == CaptureMode::Picture {
                    "Picture capture unavailable: no active camera session."
                } else {
                    "Video capture unavailable: no active camera session."
                });
                return;
            };

            match capture_mode.get() {
                CaptureMode::Picture => {
                    log_live_view(format!(
                        "picture capture requested for {}:{}",
                        camera.host, camera.port
                    ));
                    status_label.set_text("Taking picture...");
                    flush_main_context();
                    match trigger_picture_capture(&camera, &cookie) {
                        Ok(()) => {
                            let storage_mode = storage.get();
                            if storage_mode != StorageMode::CameraOnly {
                                disconnect_action.set_enabled(false);
                                status_label.set_text("Downloading...");
                                flush_main_context();
                            }
                            let result = apply_storage_policy_to_capture(
                                &camera,
                                &cookie,
                                &workspace.borrow(),
                                storage_mode,
                                CapturedMediaKind::Picture,
                            );
                            if storage_mode != StorageMode::CameraOnly {
                                disconnect_action.set_enabled(connected.get());
                            }
                            match result {
                                Ok(message) => status_label.set_text(&message),
                                Err(error) => {
                                    log_live_view(format!(
                                        "picture storage handling failed: {error}"
                                    ));
                                    status_label.set_text(&format!(
                                        "Picture captured, but storage handling failed: {error}"
                                    ));
                                }
                            }
                        }
                        Err(error) => {
                            log_live_view(format!("picture capture failed: {error}"));
                            status_label.set_text(&format!("Picture capture error: {error}"));
                        }
                    }
                }
                CaptureMode::Video => {
                    if video_recording.get() {
                        log_live_view(format!(
                            "video stop requested for {}:{}",
                            camera.host, camera.port
                        ));
                        status_label.set_text("Stopping video...");
                        flush_main_context();
                        match stop_video_recording(&camera, &cookie) {
                            Ok(()) => {
                                video_recording.set(false);
                                update_capture_mode_controls(
                                    connected.get(),
                                    capture_mode.get(),
                                    video_recording.get(),
                                    &capture_action_state,
                                    &capture_button,
                                    &capture_mode_switch,
                                );
                                let storage_mode = storage.get();
                                if storage_mode != StorageMode::CameraOnly {
                                    status_label.set_text("Downloading...");
                                    flush_main_context();
                                }
                                let result = apply_storage_policy_to_capture(
                                    &camera,
                                    &cookie,
                                    &workspace.borrow(),
                                    storage_mode,
                                    CapturedMediaKind::Video,
                                );
                                disconnect_action.set_enabled(connected.get());
                                match result {
                                    Ok(message) => status_label.set_text(&message),
                                    Err(error) => {
                                        log_live_view(format!(
                                            "video storage handling failed: {error}"
                                        ));
                                        status_label.set_text(&format!(
                                            "Video captured, but storage handling failed: {error}"
                                        ));
                                    }
                                }
                            }
                            Err(error) => {
                                log_live_view(format!("video stop failed: {error}"));
                                status_label.set_text(&format!("Video stop error: {error}"));
                            }
                        }
                    } else {
                        log_live_view(format!(
                            "video capture requested for {}:{}",
                            camera.host, camera.port
                        ));
                        status_label.set_text("Starting video...");
                        flush_main_context();
                        match start_video_recording(&camera, &cookie) {
                            Ok(()) => {
                                video_recording.set(true);
                                disconnect_action.set_enabled(false);
                                update_capture_mode_controls(
                                    connected.get(),
                                    capture_mode.get(),
                                    video_recording.get(),
                                    &capture_action_state,
                                    &capture_button,
                                    &capture_mode_switch,
                                );
                                status_label.set_text("Recording video...");
                            }
                            Err(error) => {
                                log_live_view(format!("video start failed: {error}"));
                                status_label.set_text(&format!("Video start error: {error}"));
                            }
                        }
                    }
                }
            }
        });
    }

    {
        let status_label = status_label.clone();
        let capture_action = capture_action.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode = capture_mode.clone();
        let connected = connected.clone();
        let video_recording = video_recording.clone();
        connected_view
            .capture_mode_switch
            .connect_active_notify(move |capture_mode_switch| {
                if video_recording.get() {
                    capture_mode_switch.set_active(capture_mode.get() == CaptureMode::Video);
                    return;
                }
                let mode = if capture_mode_switch.is_active() {
                    CaptureMode::Video
                } else {
                    CaptureMode::Picture
                };
                capture_mode.set(mode);
                update_capture_mode_controls(
                    connected.get(),
                    mode,
                    video_recording.get(),
                    &capture_action,
                    &capture_button,
                    capture_mode_switch,
                );
                if connected.get() {
                    status_label.set_text(if mode == CaptureMode::Picture {
                        "Picture mode selected."
                    } else {
                        "Video mode selected."
                    });
                }
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

            log_live_view(format!(
                "focus requested for {}:{}",
                camera.host, camera.port
            ));
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

    for (button, direction) in [
        (
            connected_view.focus_move_up_left.clone(),
            FocusDirection::UpLeft,
        ),
        (connected_view.focus_move_up.clone(), FocusDirection::Up),
        (
            connected_view.focus_move_up_right.clone(),
            FocusDirection::UpRight,
        ),
        (
            connected_view.focus_move_down_left.clone(),
            FocusDirection::DownLeft,
        ),
        (connected_view.focus_move_down.clone(), FocusDirection::Down),
        (
            connected_view.focus_move_down_right.clone(),
            FocusDirection::DownRight,
        ),
        (connected_view.focus_move_left.clone(), FocusDirection::Left),
        (
            connected_view.focus_move_right.clone(),
            FocusDirection::Right,
        ),
    ] {
        let status_label = status_label.clone();
        let configured_camera = configured_camera.clone();
        let live_view_session = live_view_session.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        button.connect_clicked(move |_| {
            let camera = configured_camera.borrow().clone();
            let cookie = live_view_session
                .borrow()
                .as_ref()
                .and_then(|session| session.session_cookie.lock().ok()?.clone());

            let Some(cookie) = cookie else {
                status_label.set_text("Focus point move unavailable: no active camera session.");
                return;
            };

            let new_state = {
                let current = focus_overlay_state.borrow().clone();
                match shifted_focus_overlay_state(&current, direction) {
                    Some(state) => state,
                    None => {
                        status_label.set_text("Focus point move unavailable.");
                        return;
                    }
                }
            };

            log_live_view(format!(
                "focus point move requested direction={:?} target=({}, {})",
                direction,
                focus_target_x(&new_state),
                focus_target_y(&new_state)
            ));

            match move_focus_point(&camera, &cookie, &new_state) {
                Ok(()) => {
                    *focus_overlay_state.borrow_mut() = new_state;
                    focus_overlay_area.queue_draw();
                    status_label.set_text("Focus point moved.");
                }
                Err(error) => {
                    log_live_view(format!("focus point move failed: {error}"));
                    status_label.set_text(&format!("Focus point error: {error}"));
                }
            }
        });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        connected_view
            .focus_trigger_button
            .connect_clicked(move |_| {
                let camera = configured_camera.borrow().clone();
                let cookie = live_view_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| session.session_cookie.lock().ok()?.clone());

                let Some(cookie) = cookie else {
                    status_label.set_text("Focus unavailable: no active camera session.");
                    return;
                };

                log_live_view(format!(
                    "focus requested for {}:{}",
                    camera.host, camera.port
                ));
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
        let workspace = workspace.clone();
        let storage = storage.clone();
        let status_label = status_label.clone();
        let window = window.clone();
        configuration_action.connect_activate(move |_, _| {
            present_configuration_dialog(
                &window,
                workspace.clone(),
                storage.clone(),
                configured_camera.clone(),
                &status_label,
            );
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
    root.append(&connected_view.content);
    root.append(&status_label);

    window.set_child(Some(&root));
    window.present();
    set_application_icon(&window);
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
    camera_menu.append(Some("Take Picture"), Some("app.camera-capture"));
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

    toolbar.append(&connect_button);
    toolbar.append(&disconnect_button);
    toolbar
}

fn build_content_view(focus_overlay_state: Rc<RefCell<FocusOverlayState>>) -> ConnectedView {
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

    let logo = logo_picture(LOGO_256X256);
    logo.set_halign(Align::Center);
    logo.set_valign(Align::Center);
    disconnected.append(&logo);

    let connected = GtkBox::new(Orientation::Horizontal, 16);
    connected.set_hexpand(true);
    connected.set_vexpand(true);

    let live_view_overlay = Overlay::new();
    live_view_overlay.set_hexpand(true);
    live_view_overlay.set_vexpand(true);

    let live_view_picture = Picture::new();
    live_view_picture.set_halign(Align::Fill);
    live_view_picture.set_valign(Align::Fill);
    live_view_picture.set_hexpand(true);
    live_view_picture.set_vexpand(true);
    live_view_picture.set_can_shrink(true);
    live_view_picture.set_keep_aspect_ratio(true);

    let focus_overlay_area = DrawingArea::new();
    focus_overlay_area.set_hexpand(true);
    focus_overlay_area.set_vexpand(true);
    focus_overlay_area.set_halign(Align::Fill);
    focus_overlay_area.set_valign(Align::Fill);
    {
        let focus_overlay_state = focus_overlay_state.clone();
        focus_overlay_area.set_draw_func(move |_, context, width, height| {
            draw_focus_overlay(
                context,
                width as f64,
                height as f64,
                &focus_overlay_state.borrow(),
            );
        });
    }

    live_view_overlay.set_child(Some(&live_view_picture));
    live_view_overlay.add_overlay(&focus_overlay_area);

    let side_panel = GtkBox::new(Orientation::Vertical, 12);
    side_panel.set_width_request(220);
    side_panel.set_margin_end(12);
    side_panel.set_margin_top(12);
    side_panel.set_margin_bottom(12);

    let capture_title = Label::new(Some("Capture"));
    capture_title.set_halign(Align::Start);
    capture_title.add_css_class("heading");

    let capture_mode_row = GtkBox::new(Orientation::Horizontal, 8);
    capture_mode_row.set_halign(Align::Start);

    let picture_mode_label = Label::new(Some("Picture"));
    let capture_mode_switch = Switch::new();
    capture_mode_switch.set_active(false);
    capture_mode_switch.set_tooltip_text(Some("Switch between picture and video modes"));
    let video_mode_label = Label::new(Some("Video"));

    capture_mode_row.append(&picture_mode_label);
    capture_mode_row.append(&capture_mode_switch);
    capture_mode_row.append(&video_mode_label);

    let capture_button = Button::with_label("Take Picture");
    capture_button.set_action_name(Some("app.camera-capture"));

    let panel_title = Label::new(Some("Focus point"));
    panel_title.set_halign(Align::Start);
    panel_title.add_css_class("heading");

    let focus_operation_label = Label::new(Some("AF mode: -"));
    focus_operation_label.set_halign(Align::Start);

    let focus_method_label = Label::new(Some("AF method: -"));
    focus_method_label.set_halign(Align::Start);

    let focus_grid = Grid::builder()
        .column_spacing(6)
        .row_spacing(6)
        .halign(Align::Center)
        .build();
    let focus_move_up_left = Button::with_label("↖");
    let focus_move_up = Button::with_label("↑");
    let focus_move_up_right = Button::with_label("↗");
    let focus_move_down_left = Button::with_label("↙");
    let focus_move_down = Button::with_label("↓");
    let focus_move_down_right = Button::with_label("↘");
    let focus_move_left = Button::with_label("←");
    let focus_trigger_button = Button::with_label("◎");
    let focus_move_right = Button::with_label("→");
    focus_trigger_button.set_action_name(Some("app.camera-focus"));
    focus_trigger_button.set_tooltip_text(Some("Focus"));
    focus_grid.attach(&focus_move_up_left, 0, 0, 1, 1);
    focus_grid.attach(&focus_move_up, 1, 0, 1, 1);
    focus_grid.attach(&focus_move_up_right, 2, 0, 1, 1);
    focus_grid.attach(&focus_move_left, 0, 1, 1, 1);
    focus_grid.attach(&focus_trigger_button, 1, 1, 1, 1);
    focus_grid.attach(&focus_move_right, 2, 1, 1, 1);
    focus_grid.attach(&focus_move_down_left, 0, 2, 1, 1);
    focus_grid.attach(&focus_move_down, 1, 2, 1, 1);
    focus_grid.attach(&focus_move_down_right, 2, 2, 1, 1);

    side_panel.append(&capture_title);
    side_panel.append(&capture_mode_row);
    side_panel.append(&capture_button);
    side_panel.append(&panel_title);
    side_panel.append(&focus_operation_label);
    side_panel.append(&focus_method_label);
    side_panel.append(&focus_grid);

    connected.append(&live_view_overlay);
    connected.append(&side_panel);

    stack.add_named(&disconnected, Some("disconnected"));
    stack.add_named(&connected, Some("connected"));
    content.append(&stack);
    ConnectedView {
        content,
        content_stack: stack,
        live_view_picture,
        capture_mode_switch,
        capture_button,
        focus_overlay_area,
        focus_operation_label,
        focus_method_label,
        focus_move_up_left,
        focus_move_up,
        focus_move_up_right,
        focus_move_down_left,
        focus_move_down,
        focus_move_down_right,
        focus_move_left,
        focus_trigger_button,
        focus_move_right,
    }
}

fn present_configuration_dialog(
    parent: &ApplicationWindow,
    workspace: Rc<RefCell<PathBuf>>,
    storage: Rc<Cell<StorageMode>>,
    configured_camera: Rc<RefCell<ConfiguredCamera>>,
    status_label: &Label,
) {
    let current_workspace = workspace.borrow().clone();
    let current_storage = storage.get();
    let current = configured_camera.borrow().clone();

    let dialog = Dialog::builder()
        .title("Configuration")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();
    dialog.set_default_size(680, 320);
    let cancel_button = dialog.add_button("Cancel", ResponseType::Cancel);
    let save_button = dialog.add_button("Save", ResponseType::Accept);
    for button in [&cancel_button, &save_button] {
        button.set_margin_top(12);
        button.set_margin_bottom(12);
    }
    save_button.set_margin_end(12);

    let content_area = dialog.content_area();
    content_area.set_spacing(18);
    content_area.set_margin_top(24);
    content_area.set_margin_bottom(24);
    content_area.set_margin_start(24);
    content_area.set_margin_end(24);

    let tabs = Stack::new();
    tabs.set_hexpand(true);
    tabs.set_vexpand(true);

    let switcher = StackSwitcher::new();
    switcher.set_halign(Align::Start);
    switcher.set_stack(Some(&tabs));

    let general_grid = Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .hexpand(true)
        .build();
    let workspace_entry = Entry::builder()
        .text(current_workspace.display().to_string())
        .hexpand(true)
        .build();
    let workspace_row = GtkBox::new(Orientation::Horizontal, 6);
    workspace_row.set_hexpand(true);
    let workspace_browse_button = Button::with_label("Browse...");
    workspace_row.append(&workspace_entry);
    workspace_row.append(&workspace_browse_button);
    attach_form_row(&general_grid, 0, "Workspace", &workspace_row);

    let storage_dropdown = DropDown::from_strings(&["Camera only", "Workspace only", "Both"]);
    storage_dropdown.set_selected(storage_mode_index(current_storage));
    attach_form_row(&general_grid, 1, "Storage", &storage_dropdown);

    let general_page = GtkBox::new(Orientation::Vertical, 0);
    general_page.set_margin_top(12);
    general_page.append(&general_grid);

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

    let camera_page = GtkBox::new(Orientation::Vertical, 0);
    camera_page.set_margin_top(12);
    camera_page.append(&grid);

    tabs.add_titled(&general_page, Some("general"), "General");
    tabs.add_titled(&camera_page, Some("camera"), "Camera");
    content_area.append(&switcher);
    content_area.append(&tabs);

    {
        let parent = parent.clone();
        let workspace_entry = workspace_entry.clone();
        workspace_browse_button.connect_clicked(move |_| {
            let chooser = FileChooserNative::builder()
                .title("Select Workspace")
                .transient_for(&parent)
                .modal(true)
                .action(FileChooserAction::SelectFolder)
                .accept_label("Select")
                .cancel_label("Cancel")
                .build();

            let current_path = workspace_entry.text();
            let current_path = current_path.trim();
            if !current_path.is_empty() {
                let folder = gio::File::for_path(current_path);
                let _ = chooser.set_current_folder(Some(&folder));
            }

            let workspace_entry = workspace_entry.clone();
            chooser.connect_response(move |chooser, response| {
                if response == ResponseType::Accept
                    && let Some(folder) = chooser.file()
                    && let Some(path) = folder.path()
                {
                    workspace_entry.set_text(&path.to_string_lossy());
                }
                chooser.hide();
            });
            chooser.show();
        });
    }

    let workspace_state = workspace.clone();
    let storage_state = storage.clone();
    let configured_camera_state = configured_camera.clone();
    let status_label = status_label.clone();
    dialog.connect_response(move |dialog, response| {
        if response != ResponseType::Accept {
            dialog.close();
            return;
        }

        let workspace_text = workspace_entry.text().trim().to_owned();
        let camera_name = camera_name_entry.text().trim().to_owned();
        let host = host_entry.text().trim().to_owned();
        let username = optional_entry_text(&username_entry);
        let password = optional_entry_text(&password_entry);

        if workspace_text.is_empty() {
            status_label.set_text("Configuration requires a workspace.");
            return;
        }

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
        let workspace = PathBuf::from(workspace_text);
        let storage = storage_mode_from_index(storage_dropdown.selected());

        match save_configuration(&workspace, storage, &camera) {
            Ok(()) => {
                *workspace_state.borrow_mut() = workspace;
                storage_state.set(storage);
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

    let logo = logo_picture(LOGO_256X256);
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

fn save_configuration(
    workspace: &Path,
    storage: StorageMode,
    camera: &ConfiguredCamera,
) -> io::Result<()> {
    let path = config::user_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    config::write_user_config(&path, workspace, storage, camera)
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

fn initial_workspace(config: Option<&AppConfig>) -> PathBuf {
    config
        .map(|app_config| app_config.workspace().to_path_buf())
        .unwrap_or_else(config::default_workspace)
}

fn initial_storage(config: Option<&AppConfig>) -> StorageMode {
    config
        .map(AppConfig::storage)
        .unwrap_or_else(config::default_storage)
}

fn storage_mode_index(storage: StorageMode) -> u32 {
    match storage {
        StorageMode::CameraOnly => 0,
        StorageMode::WorkspaceOnly => 1,
        StorageMode::Both => 2,
    }
}

fn storage_mode_from_index(index: u32) -> StorageMode {
    match index {
        0 => StorageMode::CameraOnly,
        2 => StorageMode::Both,
        _ => StorageMode::WorkspaceOnly,
    }
}

fn update_connection_state(
    connected: bool,
    status_label: &Label,
    connect_action: &gio::SimpleAction,
    disconnect_action: &gio::SimpleAction,
    capture_action: &gio::SimpleAction,
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
    capture_action.set_enabled(connected);
    focus_action.set_enabled(connected);
    content_stack.set_visible_child_name(if connected {
        "connected"
    } else {
        "disconnected"
    });
}

fn update_capture_mode_controls(
    connected: bool,
    capture_mode: CaptureMode,
    video_recording: bool,
    capture_action: &gio::SimpleAction,
    capture_button: &Button,
    capture_mode_switch: &Switch,
) {
    capture_mode_switch.set_sensitive(connected && !video_recording);
    match capture_mode {
        CaptureMode::Picture => {
            capture_button.set_label("Take Picture");
            capture_button.set_tooltip_text(Some("Capture a still image"));
            capture_button.set_sensitive(connected);
            capture_action.set_enabled(connected);
        }
        CaptureMode::Video => {
            if video_recording {
                capture_button.set_label("Stop Video");
                capture_button.set_tooltip_text(Some("Stop video recording"));
            } else {
                capture_button.set_label("Take Video");
                capture_button.set_tooltip_text(Some("Start video recording"));
            }
            capture_button.set_sensitive(connected);
            capture_action.set_enabled(connected);
        }
    }
}

fn logo_picture(bytes: &'static [u8]) -> Picture {
    match logo_texture(bytes) {
        Ok(texture) => Picture::for_paintable(&texture),
        Err(error) => {
            eprintln!("[argus-capture] failed to load embedded logo: {}", error);
            Picture::new()
        }
    }
}

fn logo_texture(bytes: &'static [u8]) -> Result<gtk::gdk::Texture, glib::Error> {
    let loader = PixbufLoader::with_type("png")?;
    loader.write(bytes)?;
    loader.close()?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| glib::Error::new(glib::FileError::Failed, "missing decoded logo pixbuf"))?;
    Ok(gtk::gdk::Texture::for_pixbuf(&pixbuf))
}

fn logo_icon_textures() -> Vec<gtk::gdk::Texture> {
    [
        LOGO_16X16,
        LOGO_32X32,
        LOGO_64X64,
        LOGO_128X128,
        LOGO_256X256,
        LOGO_512X512,
    ]
    .into_iter()
    .filter_map(|bytes| match logo_texture(bytes) {
        Ok(texture) => Some(texture),
        Err(error) => {
            eprintln!(
                "[argus-capture] failed to load embedded application icon: {}",
                error
            );
            None
        }
    })
    .collect()
}

fn set_application_icon(window: &ApplicationWindow) {
    use gtk::gdk::prelude::ToplevelExt;

    let Some(surface) = window.surface() else {
        return;
    };
    let Ok(toplevel) = surface.dynamic_cast::<gtk::gdk::Toplevel>() else {
        return;
    };

    let textures = logo_icon_textures();

    if !textures.is_empty() {
        toplevel.set_icon_list(&textures);
    }
}

fn start_live_view_session(
    configured_camera: ConfiguredCamera,
    live_view_picture: Picture,
    status_label: Label,
    rendered_frame_count: Rc<Cell<u64>>,
    focus_overlay_state: Rc<RefCell<FocusOverlayState>>,
    focus_overlay_area: DrawingArea,
    focus_operation_label: Label,
    focus_method_label: Label,
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
                LiveViewEvent::FocusOverlay(state) => {
                    *focus_overlay_state.borrow_mut() = state;
                    focus_overlay_area.queue_draw();
                }
                LiveViewEvent::FocusMode(state) => {
                    focus_operation_label.set_text(&format!("AF mode: {}", state.operation));
                    focus_method_label.set_text(&format!("AF method: {}", state.method));
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

    prepare_browser_remote_shooting_page(&base_url, &session_cookie, &sender)?;
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
        return Err(format!(
            "unexpected Browser Remote landing page `{location}`"
        ));
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

fn prepare_browser_remote_shooting_page(
    base_url: &str,
    session_cookie: &str,
    sender: &mpsc::Sender<LiveViewEvent>,
) -> Result<(), String> {
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
        if let Some(state) = parse_focus_mode_state(&body) {
            let _ = sender.send(LiveViewEvent::FocusMode(state));
        }
        Ok(())
    } else {
        Err(format!(
            "currentproperty request failed with {status}: {body}"
        ))
    }
}

fn parse_focus_mode_state(body: &str) -> Option<FocusModeState> {
    let value: Value = serde_json::from_str(body).ok()?;
    Some(FocusModeState {
        operation: value
            .get("afoperation")
            .and_then(|node| node.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_owned(),
        method: value
            .get("afmethod")
            .and_then(|node| node.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_owned(),
    })
}

fn stream_live_view(
    base_url: &str,
    session_cookie: &str,
    sender: mpsc::Sender<LiveViewEvent>,
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
) -> Result<(), String> {
    let referer = format!("{base_url}/wpd/shoot.shtml");
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
        ])
        .arg(format!("{base_url}{LIVE_VIEW_STREAM}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start live view stream: {error}"))?;
    if let Ok(mut pid_slot) = child_pid.lock() {
        *pid_slot = Some(child.id());
    }
    log_live_view(format!("spawned curl live-view process pid={}", child.id()));

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing live view stdout stream".to_owned())?;
    let mut stderr = child.stderr.take();
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut parsed_frames = 0_u64;
    let mut inspected_start = false;

    while !stop.load(Ordering::Relaxed) {
        let read = stdout.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            log_live_view("live-view stream reached EOF");
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if !inspected_start && !buffer.is_empty() {
            inspected_start = true;
            if buffer.starts_with(b"{") {
                let body = String::from_utf8_lossy(&buffer).to_string();
                log_live_view(format!(
                    "live-view stream returned JSON body instead of frames: {}",
                    preview_text(&body)
                ));
                let _ = child.kill();
                let _ = child.wait();
                if let Ok(mut pid_slot) = child_pid.lock() {
                    *pid_slot = None;
                }
                return Err(format!("live view stream returned body: {body}"));
            }
            log_live_view(format!(
                "live-view stream started with binary payload prefix={}",
                buffer[..buffer.len().min(16)]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
        }
        drain_live_view_frames(&mut buffer, &sender, &mut parsed_frames);
    }

    let _ = child.kill();
    let _ = child.wait();
    if let Ok(mut pid_slot) = child_pid.lock() {
        *pid_slot = None;
    }
    if !stop.load(Ordering::Relaxed) && parsed_frames == 0 {
        let mut err = String::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut err);
        }
        log_live_view(format!(
            "live-view stream ended without decoded frames body_prefix={} stderr_prefix={}",
            preview_text(&String::from_utf8_lossy(&buffer)),
            preview_text(&err)
        ));
        return Err("live view stream ended without decoded frames".to_owned());
    }
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
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|error| error.to_string())?;

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

fn trigger_picture_capture(camera: &ConfiguredCamera, session_cookie: &str) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/shutterbutton"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"af":true}"#),
    )?;
    log_live_view(format!(
        "picture capture status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!("picture capture failed with {status}: {body}"))
    }
}

fn start_video_recording(camera: &ConfiguredCamera, session_cookie: &str) -> Result<(), String> {
    set_movie_mode(camera, session_cookie, true)?;
    thread::sleep(Duration::from_millis(500));

    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/recbutton"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"action":"start"}"#),
    )?;
    log_live_view(format!(
        "video start status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!("video start failed with {status}: {body}"))
    }
}

fn stop_video_recording(camera: &ConfiguredCamera, session_cookie: &str) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/recbutton"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"action":"stop"}"#),
    )?;
    log_live_view(format!(
        "video stop status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!("video stop failed with {status}: {body}"))
    }
}

fn set_movie_mode(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    enabled: bool,
) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let action = if enabled { "on" } else { "off" };
    let body = format!(r#"{{"action":"{action}"}}"#);
    let (status, response_body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/control/moviemode"),
        Some(session_cookie),
        Some(&referer),
        Some(&body),
    )?;
    log_live_view(format!(
        "movie mode status={status} action={action} body_prefix={}",
        preview_text(&response_body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!(
            "movie mode `{action}` failed with {status}: {response_body}"
        ))
    }
}

fn apply_storage_policy_to_capture(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    workspace: &Path,
    storage: StorageMode,
    media_kind: CapturedMediaKind,
) -> Result<String, String> {
    if storage == StorageMode::CameraOnly {
        return Ok(match media_kind {
            CapturedMediaKind::Picture => "Picture captured on camera.".to_owned(),
            CapturedMediaKind::Video => "Video captured on camera.".to_owned(),
        });
    }

    let contents = wait_for_added_contents(camera, session_cookie, media_kind)?;
    let downloaded_paths =
        download_contents_to_workspace(camera, session_cookie, workspace, &contents)?;

    if storage == StorageMode::WorkspaceOnly {
        for content in &contents {
            delete_camera_content(camera, session_cookie, content)?;
        }
    }

    let file_count = downloaded_paths.len();
    let noun = match media_kind {
        CapturedMediaKind::Picture => {
            if file_count == 1 {
                "Picture"
            } else {
                "Pictures"
            }
        }
        CapturedMediaKind::Video => {
            if file_count == 1 {
                "Video"
            } else {
                "Videos"
            }
        }
    };
    let destination = workspace.display();

    Ok(match storage {
        StorageMode::CameraOnly => unreachable!("camera only is returned early"),
        StorageMode::WorkspaceOnly => {
            format!("{noun} downloaded to {destination} and removed from camera.")
        }
        StorageMode::Both => format!("{noun} captured on camera and downloaded to {destination}."),
    })
}

fn wait_for_added_contents(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    media_kind: CapturedMediaKind,
) -> Result<Vec<String>, String> {
    const MAX_ATTEMPTS: usize = 20;
    const POLL_DELAY: Duration = Duration::from_millis(500);

    for attempt in 1..=MAX_ATTEMPTS {
        let contents = poll_added_contents(camera, session_cookie)?;
        let matching_contents = filter_added_contents_by_media_kind(contents, media_kind);
        if !matching_contents.is_empty() {
            log_live_view(format!(
                "capture contents detected after poll #{attempt}: {}",
                matching_contents.join(", ")
            ));
            return Ok(matching_contents);
        }
        thread::sleep(POLL_DELAY);
    }

    Err("timed out waiting for captured content to appear on the camera".to_owned())
}

fn poll_added_contents(
    camera: &ConfiguredCamera,
    session_cookie: &str,
) -> Result<Vec<String>, String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "GET",
        &format!("{base_url}/ccapi/ver100/event/polling?continue=on"),
        Some(session_cookie),
        Some(&referer),
        None,
    )?;
    log_live_view(format!(
        "event polling status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status != 200 {
        return Err(format!("event polling failed with {status}: {body}"));
    }

    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("failed to parse event polling response: {error}"))?;

    Ok(value
        .get("addedcontents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

fn filter_added_contents_by_media_kind(
    contents: Vec<String>,
    media_kind: CapturedMediaKind,
) -> Vec<String> {
    contents
        .into_iter()
        .filter(|content| match media_kind {
            CapturedMediaKind::Picture => {
                has_content_extension(content, &["jpg", "jpeg", "hif", "heif", "cr2", "cr3"])
            }
            CapturedMediaKind::Video => has_content_extension(content, &["mp4", "mov", "crm"]),
        })
        .collect()
}

fn has_content_extension(content_path: &str, extensions: &[&str]) -> bool {
    let lower = content_path.to_ascii_lowercase();
    extensions
        .iter()
        .any(|extension| lower.ends_with(&format!(".{extension}")))
}

fn download_contents_to_workspace(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    workspace: &Path,
    contents: &[String],
) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;

    let mut downloaded_paths = Vec::with_capacity(contents.len());
    for content in contents {
        downloaded_paths.push(download_camera_content(
            camera,
            session_cookie,
            workspace,
            content,
        )?);
    }
    Ok(downloaded_paths)
}

fn download_camera_content(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    workspace: &Path,
    content_path: &str,
) -> Result<PathBuf, String> {
    const MAX_ATTEMPTS: usize = 20;
    const RETRY_DELAY: Duration = Duration::from_millis(500);

    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let file_name = content_file_name(content_path)?;
    let destination = next_available_workspace_path(workspace, file_name);
    let content_url = camera_content_url(&base_url, content_path, Some("main"));

    for attempt in 1..=MAX_ATTEMPTS {
        let output = Command::new("curl")
            .args([
                "-sS",
                "-H",
                &format!("Cookie: {session_cookie}"),
                "-e",
                &referer,
                "-o",
                &destination.to_string_lossy(),
                "-w",
                "__STATUS__:%{http_code}",
            ])
            .arg(&content_url)
            .output()
            .map_err(|error| format!("failed to download `{content_path}`: {error}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let status = stdout
            .trim()
            .strip_prefix("__STATUS__:")
            .ok_or_else(|| "download response missing status marker".to_owned())?
            .parse::<u16>()
            .map_err(|error| format!("invalid download status code: {error}"))?;

        if output.status.success() && status == 200 {
            log_live_view(format!(
                "downloaded camera content `{content_path}` to {} on attempt #{attempt}",
                destination.display()
            ));
            return Ok(destination);
        }

        let _ = fs::remove_file(&destination);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        log_live_view(format!(
            "download attempt #{attempt} failed for `{content_path}` status={status} stderr_prefix={}",
            preview_text(&stderr)
        ));

        if attempt < MAX_ATTEMPTS && matches!(status, 404 | 409 | 503) {
            thread::sleep(RETRY_DELAY);
            continue;
        }

        return Err(format!(
            "download failed for `{content_path}` with status {status}: {stderr}"
        ));
    }

    unreachable!("download loop always returns on success or final failure")
}

fn delete_camera_content(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    content_path: &str,
) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "DELETE",
        &format!("{base_url}{content_path}"),
        Some(session_cookie),
        Some(&referer),
        None,
    )?;
    log_live_view(format!(
        "delete content status={status} path={content_path} body_prefix={}",
        preview_text(&body)
    ));

    if matches!(status, 200 | 204) {
        Ok(())
    } else {
        Err(format!(
            "delete failed for `{content_path}` with {status}: {body}"
        ))
    }
}

fn content_file_name(content_path: &str) -> Result<&str, String> {
    content_path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .ok_or_else(|| format!("content path `{content_path}` does not contain a file name"))
}

fn next_available_workspace_path(workspace: &Path, file_name: &str) -> PathBuf {
    let base_path = workspace.join(file_name);
    if !base_path.exists() {
        return base_path;
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name);
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str());

    for index in 1.. {
        let candidate_name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate_path = workspace.join(candidate_name);
        if !candidate_path.exists() {
            return candidate_path;
        }
    }

    unreachable!("incrementing candidate paths always returns")
}

fn camera_content_url(base_url: &str, content_path: &str, kind: Option<&str>) -> String {
    let mut url = if content_path.starts_with("http://") || content_path.starts_with("https://") {
        content_path.to_owned()
    } else if content_path.starts_with('/') {
        format!("{base_url}{content_path}")
    } else {
        format!("{base_url}/{content_path}")
    };

    if let Some(kind) = kind {
        let separator = if url.contains('?') { '&' } else { '?' };
        url.push(separator);
        url.push_str("kind=");
        url.push_str(kind);
    }

    url
}

fn flush_main_context() {
    let context = glib::MainContext::default();
    while context.pending() {
        let _ = context.iteration(false);
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
        if buffer[cursor + 2] == 1
            && let Some(overlay_state) = parse_focus_overlay_state(payload)
        {
            let _ = sender.send(LiveViewEvent::FocusOverlay(overlay_state));
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

fn parse_focus_overlay_state(payload: &[u8]) -> Option<FocusOverlayState> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let live_view_data = value.get("liveviewdata")?;
    let image = live_view_data.get("image")?;
    let frames = live_view_data.get("afframe")?.as_array()?;

    let selected_frame = frames
        .iter()
        .filter(|frame| frame.get("select").and_then(Value::as_i64) == Some(1))
        .filter_map(|frame| {
            let width = frame.get("width")?.as_f64()?;
            let height = frame.get("height")?.as_f64()?;
            Some((width * height, frame))
        })
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, frame)| frame)
        .or_else(|| {
            frames
                .iter()
                .filter_map(|frame| {
                    let width = frame.get("width")?.as_f64()?;
                    let height = frame.get("height")?.as_f64()?;
                    Some((width * height, frame))
                })
                .max_by(|left, right| {
                    left.0
                        .partial_cmp(&right.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(_, frame)| frame)
        })?;

    Some(FocusOverlayState {
        image_x: image.get("positionx")?.as_f64()?,
        image_y: image.get("positiony")?.as_f64()?,
        image_width: image.get("positionwidth")?.as_f64()?,
        image_height: image.get("positionheight")?.as_f64()?,
        frame_x: selected_frame.get("x")?.as_f64()?,
        frame_y: selected_frame.get("y")?.as_f64()?,
        frame_width: selected_frame.get("width")?.as_f64()?,
        frame_height: selected_frame.get("height")?.as_f64()?,
        active: true,
    })
}

fn draw_focus_overlay(
    context: &gtk::cairo::Context,
    width: f64,
    height: f64,
    state: &FocusOverlayState,
) {
    if !state.active || state.image_width <= 0.0 || state.image_height <= 0.0 {
        return;
    }

    let (offset_x, offset_y, scaled_width, scaled_height) =
        contained_image_rect(width, height, state.image_width, state.image_height);
    let scale_x = scaled_width / state.image_width;
    let scale_y = scaled_height / state.image_height;
    let x = offset_x + (state.frame_x - state.image_x) * scale_x;
    let y = offset_y + (state.frame_y - state.image_y) * scale_y;
    let frame_width = state.frame_width * scale_x;
    let frame_height = state.frame_height * scale_y;

    context.set_source_rgba(1.0, 0.2, 0.2, 0.9);
    context.set_line_width(2.0);
    context.rectangle(x, y, frame_width, frame_height);
    let _ = context.stroke();
}

fn contained_image_rect(
    available_width: f64,
    available_height: f64,
    image_width: f64,
    image_height: f64,
) -> (f64, f64, f64, f64) {
    let scale = (available_width / image_width).min(available_height / image_height);
    let scaled_width = image_width * scale;
    let scaled_height = image_height * scale;
    let offset_x = (available_width - scaled_width) / 2.0;
    let offset_y = (available_height - scaled_height) / 2.0;
    (offset_x, offset_y, scaled_width, scaled_height)
}

fn shifted_focus_overlay_state(
    state: &FocusOverlayState,
    direction: FocusDirection,
) -> Option<FocusOverlayState> {
    if !state.active || state.image_width <= 0.0 || state.image_height <= 0.0 {
        return None;
    }

    let step_x = state.frame_width.max(64.0);
    let step_y = state.frame_height.max(64.0);
    let mut next = state.clone();

    match direction {
        FocusDirection::UpLeft => {
            next.frame_x -= step_x;
            next.frame_y -= step_y;
        }
        FocusDirection::Up => next.frame_y -= step_y,
        FocusDirection::UpRight => {
            next.frame_x += step_x;
            next.frame_y -= step_y;
        }
        FocusDirection::Down => next.frame_y += step_y,
        FocusDirection::DownLeft => {
            next.frame_x -= step_x;
            next.frame_y += step_y;
        }
        FocusDirection::DownRight => {
            next.frame_x += step_x;
            next.frame_y += step_y;
        }
        FocusDirection::Left => next.frame_x -= step_x,
        FocusDirection::Right => next.frame_x += step_x,
    }

    next.frame_x = next.frame_x.clamp(
        next.image_x,
        next.image_x + next.image_width - next.frame_width,
    );
    next.frame_y = next.frame_y.clamp(
        next.image_y,
        next.image_y + next.image_height - next.frame_height,
    );
    Some(next)
}

fn focus_target_x(state: &FocusOverlayState) -> i32 {
    (state.frame_x + state.frame_width / 2.0).round() as i32
}

fn focus_target_y(state: &FocusOverlayState) -> i32 {
    (state.frame_y + state.frame_height / 2.0).round() as i32
}

fn move_focus_point(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    state: &FocusOverlayState,
) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let body = format!(
        "{{\"positionx\":{},\"positiony\":{}}}",
        focus_target_x(state),
        focus_target_y(state)
    );

    let (status, response_body) = run_curl_request(
        "PUT",
        &format!("{base_url}/ccapi/ver100/shooting/liveview/afframeposition"),
        Some(session_cookie),
        Some(&referer),
        Some(&body),
    )?;
    log_live_view(format!(
        "focus point move status={status} body_prefix={}",
        preview_text(&response_body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!(
            "focus point move failed with {status}: {response_body}"
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_picture_contents_by_extension() {
        let contents = vec![
            "/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG".to_owned(),
            "/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.CR3".to_owned(),
            "/ccapi/ver130/contents/card1/DCIM/100CANON/MVI_0001.MP4".to_owned(),
        ];

        let filtered = filter_added_contents_by_media_kind(contents, CapturedMediaKind::Picture);

        assert_eq!(
            filtered,
            vec![
                "/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG".to_owned(),
                "/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.CR3".to_owned(),
            ]
        );
    }

    #[test]
    fn picks_unique_workspace_path_when_file_exists() {
        let temp_dir =
            std::env::temp_dir().join(format!("argus-capture-gui-test-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let existing_path = temp_dir.join("IMG_0001.JPG");
        fs::write(&existing_path, b"existing").unwrap();

        let next_path = next_available_workspace_path(&temp_dir, "IMG_0001.JPG");

        assert_eq!(next_path, temp_dir.join("IMG_0001-1.JPG"));

        fs::remove_file(existing_path).unwrap();
        fs::remove_dir(temp_dir).unwrap();
    }

    #[test]
    fn appends_kind_to_relative_camera_content_url() {
        assert_eq!(
            camera_content_url(
                "http://camera.local:8080",
                "/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG",
                Some("main")
            ),
            "http://camera.local:8080/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG?kind=main"
        );
    }

    #[test]
    fn appends_kind_to_absolute_camera_content_url() {
        assert_eq!(
            camera_content_url(
                "http://camera.local:8080",
                "http://camera.local:8080/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG",
                Some("main")
            ),
            "http://camera.local:8080/ccapi/ver130/contents/card1/DCIM/100CANON/IMG_0001.JPG?kind=main"
        );
    }
}
