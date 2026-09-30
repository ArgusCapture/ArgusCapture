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
use std::collections::HashMap;
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
    Entry, FileChooserAction, FileChooserNative, GestureClick, Grid, Label, ListBox, Orientation,
    Overlay, Picture, PopoverMenuBar, ResponseType, ScrolledWindow, SpinButton, Stack,
    StackSwitcher, StringList, Switch,
};
use serde_json::Value;
use tokio::runtime::Builder;

use crate::config::{self, AppConfig, ConfiguredCamera, StorageMode};
use crate::network::{self, NetworkCamera};

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
const PICTURE_CONTENT_EXTENSIONS: &[&str] = &["jpg", "jpeg", "hif", "heif", "cr2", "cr3"];
const VIDEO_CONTENT_EXTENSIONS: &[&str] = &["mp4", "mov", "crm"];

struct LiveViewSession {
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<u32>>>,
    session_cookie: Arc<Mutex<Option<String>>>,
    pending_added_contents: Arc<Mutex<Vec<String>>>,
    ui_source: SourceId,
    worker: thread::JoinHandle<()>,
}

struct LiveViewUiBindings {
    live_view_picture: Picture,
    status_label: Label,
    connect_action: gio::SimpleAction,
    disconnect_action: gio::SimpleAction,
    capture_action: gio::SimpleAction,
    focus_action: gio::SimpleAction,
    content_stack: Stack,
    startup_logo: Picture,
    startup_blink_source: Rc<RefCell<Option<SourceId>>>,
    rendered_frame_count: Rc<Cell<u64>>,
    focus_overlay_state: Rc<RefCell<FocusOverlayState>>,
    focus_overlay_area: DrawingArea,
    focus_operation_label: Label,
    focus_method_label: Label,
    mode_label: Label,
    mode_dropdown: DropDown,
    iso_label: Label,
    iso_dropdown: DropDown,
    shutter_speed_label: Label,
    shutter_speed_dropdown: DropDown,
    current_shutter_speed_display: Label,
    aperture_label: Label,
    aperture_dropdown: DropDown,
    mode_dropdown_updating: Rc<Cell<bool>>,
    iso_dropdown_updating: Rc<Cell<bool>>,
    shutter_speed_dropdown_updating: Rc<Cell<bool>>,
    aperture_dropdown_updating: Rc<Cell<bool>>,
    capture_settings_cache: Rc<RefCell<Option<CaptureSettingsCache>>>,
}

#[derive(Clone)]
struct CaptureSettingsControls {
    mode_label: Label,
    mode_dropdown: DropDown,
    iso_label: Label,
    iso_dropdown: DropDown,
    shutter_speed_label: Label,
    shutter_speed_dropdown: DropDown,
    current_shutter_speed_display: Label,
    aperture_label: Label,
    aperture_dropdown: DropDown,
    mode_dropdown_updating: Rc<Cell<bool>>,
    iso_dropdown_updating: Rc<Cell<bool>>,
    shutter_speed_dropdown_updating: Rc<Cell<bool>>,
    aperture_dropdown_updating: Rc<Cell<bool>>,
}

#[derive(Clone, Debug, Default)]
struct CaptureSettingsCache {
    current_mode: String,
    by_mode: HashMap<String, CaptureSettingsState>,
}

enum LiveViewEvent {
    Frame(Vec<u8>),
    FocusOverlay(FocusOverlayState),
    FocusMode(FocusModeState),
    CaptureSettings(CaptureSettingsState),
    CaptureSettingsCache(CaptureSettingsCache),
    EffectiveExposure {
        shutter_speed: Option<String>,
        aperture: Option<String>,
    },
    Error(String),
}

struct ConnectedView {
    content: GtkBox,
    content_stack: Stack,
    startup_logo: Picture,
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
    mode_label: Label,
    mode_dropdown: DropDown,
    iso_label: Label,
    iso_dropdown: DropDown,
    shutter_speed_label: Label,
    shutter_speed_dropdown: DropDown,
    current_shutter_speed_display: Label,
    aperture_label: Label,
    aperture_dropdown: DropDown,
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

#[derive(Clone, Debug, Default)]
struct SelectableSettingState {
    current: String,
    ability: Vec<String>,
}

impl SelectableSettingState {
    fn is_available(&self) -> bool {
        !self.ability.is_empty() || !self.current.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
struct CaptureSettingsState {
    mode: SelectableSettingState,
    iso: SelectableSettingState,
    shutter_speed: SelectableSettingState,
    aperture: SelectableSettingState,
    // Camera-metered values (`effective_value_tv` / `effective_value_av`);
    // these are the only exposure values the camera reports for parameters it
    // estimates itself (shutter speed in P/Av/auto, aperture in P/Tv/auto).
    effective_shutter_speed: String,
    effective_aperture: String,
}

impl CaptureSettingsState {
    fn display_shutter_speed(&self) -> &str {
        let selected = self.shutter_speed.current.trim();
        if selected.is_empty() {
            self.effective_shutter_speed.trim()
        } else {
            selected
        }
    }

    fn display_aperture(&self) -> &str {
        let selected = self.aperture.current.trim();
        let aperture = if selected.is_empty() {
            self.effective_aperture.trim()
        } else {
            selected
        };
        aperture.strip_prefix('f').unwrap_or(aperture)
    }

    // Exposure summary for the shutter speed panel, e.g. "2.0 1/640".
    fn display_exposure(&self) -> String {
        let aperture = self.display_aperture();
        let shutter_speed = self.display_shutter_speed();
        match (aperture.is_empty(), shutter_speed.is_empty()) {
            (false, false) => format!("{aperture} {shutter_speed}"),
            (true, false) => shutter_speed.to_owned(),
            (false, true) => aperture.to_owned(),
            (true, true) => String::new(),
        }
    }

    fn has_complete_exposure(&self) -> bool {
        !self.display_shutter_speed().is_empty() && !self.display_aperture().is_empty()
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentViewState {
    Disconnected,
    Starting,
    Connected,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturedMediaKind {
    Picture,
    Video,
}

struct CaptureOutcome {
    status_message: String,
    path_labels: Vec<String>,
}

#[derive(Debug, Eq, PartialEq)]
enum BrowserRemoteLoginError {
    MissingCredentials,
    Curl(String),
    AlreadyInUse,
    UnexpectedLandingPage(String),
    MissingSessionCookie,
}

impl BrowserRemoteLoginError {
    fn into_message(self) -> String {
        match self {
            Self::MissingCredentials => "Browser Remote requires username and password".to_owned(),
            Self::Curl(message) => message,
            Self::AlreadyInUse => "Browser Remote is already in use".to_owned(),
            Self::UnexpectedLandingPage(location) => {
                format!("unexpected Browser Remote landing page `{location}`")
            }
            Self::MissingSessionCookie => "missing Browser Remote session cookie".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveViewEnableOutcome {
    Enabled,
    Busy,
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
    let mode_dropdown_updating = Rc::new(Cell::new(false));
    let iso_dropdown_updating = Rc::new(Cell::new(false));
    let shutter_speed_dropdown_updating = Rc::new(Cell::new(false));
    let aperture_dropdown_updating = Rc::new(Cell::new(false));
    let connect_action = gio::SimpleAction::new("camera-connect", None);
    let disconnect_action = gio::SimpleAction::new("camera-disconnect", None);
    let capture_action = gio::SimpleAction::new("camera-capture", None);
    let focus_action = gio::SimpleAction::new("camera-focus", None);
    let configuration_action = gio::SimpleAction::new("edit-configuration", None);
    let album_pictures_action = gio::SimpleAction::new("album-pictures", None);
    let album_videos_action = gio::SimpleAction::new("album-videos", None);
    let about_action = gio::SimpleAction::new("help-about", None);
    let quit_action = gio::SimpleAction::new("quit", None);

    application.add_action(&connect_action);
    application.add_action(&disconnect_action);
    application.add_action(&capture_action);
    application.add_action(&focus_action);
    application.add_action(&configuration_action);
    application.add_action(&album_pictures_action);
    application.add_action(&album_videos_action);
    application.add_action(&about_action);
    application.add_action(&quit_action);

    application.set_accels_for_action("app.quit", &["q"]);
    application.set_accels_for_action("app.camera-connect", &["c"]);
    application.set_accels_for_action("app.camera-disconnect", &["d"]);
    application.set_accels_for_action("app.camera-capture", &["p"]);
    application.set_accels_for_action("app.camera-focus", &["f"]);
    application.set_accels_for_action("app.help-about", &["a"]);

    let connected_view = build_content_view(focus_overlay_state.clone());
    let capture_settings_controls = CaptureSettingsControls {
        mode_label: connected_view.mode_label.clone(),
        mode_dropdown: connected_view.mode_dropdown.clone(),
        iso_label: connected_view.iso_label.clone(),
        iso_dropdown: connected_view.iso_dropdown.clone(),
        shutter_speed_label: connected_view.shutter_speed_label.clone(),
        shutter_speed_dropdown: connected_view.shutter_speed_dropdown.clone(),
        current_shutter_speed_display: connected_view.current_shutter_speed_display.clone(),
        aperture_label: connected_view.aperture_label.clone(),
        aperture_dropdown: connected_view.aperture_dropdown.clone(),
        mode_dropdown_updating: mode_dropdown_updating.clone(),
        iso_dropdown_updating: iso_dropdown_updating.clone(),
        shutter_speed_dropdown_updating: shutter_speed_dropdown_updating.clone(),
        aperture_dropdown_updating: aperture_dropdown_updating.clone(),
    };
    let capture_settings_cache: Rc<RefCell<Option<CaptureSettingsCache>>> =
        Rc::new(RefCell::new(None));
    let startup_blink_source: Rc<RefCell<Option<SourceId>>> = Rc::new(RefCell::new(None));
    update_connection_state(
        ContentViewState::Disconnected,
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
        let window = window.clone();
        let workspace = workspace.clone();
        let status_label = status_label.clone();
        album_pictures_action.connect_activate(move |_, _| {
            present_album_dialog(
                &window,
                CapturedMediaKind::Picture,
                &workspace.borrow(),
                &status_label,
            );
        });
    }

    {
        let window = window.clone();
        let workspace = workspace.clone();
        let status_label = status_label.clone();
        album_videos_action.connect_activate(move |_, _| {
            present_album_dialog(
                &window,
                CapturedMediaKind::Video,
                &workspace.borrow(),
                &status_label,
            );
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
        let startup_logo = connected_view.startup_logo.clone();
        let live_view_picture = connected_view.live_view_picture.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode_switch = connected_view.capture_mode_switch.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        let focus_operation_label = connected_view.focus_operation_label.clone();
        let focus_method_label = connected_view.focus_method_label.clone();
        let mode_label = connected_view.mode_label.clone();
        let mode_dropdown = connected_view.mode_dropdown.clone();
        let iso_label = connected_view.iso_label.clone();
        let iso_dropdown = connected_view.iso_dropdown.clone();
        let shutter_speed_label = connected_view.shutter_speed_label.clone();
        let shutter_speed_dropdown = connected_view.shutter_speed_dropdown.clone();
        let current_shutter_speed_display = connected_view.current_shutter_speed_display.clone();
        let aperture_label = connected_view.aperture_label.clone();
        let aperture_dropdown = connected_view.aperture_dropdown.clone();
        let live_view_session = live_view_session.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let capture_mode = capture_mode.clone();
        let video_recording = video_recording.clone();
        let connected = connected.clone();
        let mode_dropdown_updating = mode_dropdown_updating.clone();
        let iso_dropdown_updating = iso_dropdown_updating.clone();
        let shutter_speed_dropdown_updating = shutter_speed_dropdown_updating.clone();
        let aperture_dropdown_updating = aperture_dropdown_updating.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        let startup_blink_source = startup_blink_source.clone();
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
            *capture_settings_cache.borrow_mut() = None;
            start_starting_animation(&startup_logo, &startup_blink_source);
            update_connection_state(
                ContentViewState::Starting,
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
                LiveViewUiBindings {
                    live_view_picture: live_view_picture.clone(),
                    status_label: status_label.clone(),
                    connect_action: connect_action_state.clone(),
                    disconnect_action: disconnect_action_state.clone(),
                    capture_action: capture_action_state.clone(),
                    focus_action: focus_action_state.clone(),
                    content_stack: content_stack.clone(),
                    startup_logo: startup_logo.clone(),
                    startup_blink_source: startup_blink_source.clone(),
                    rendered_frame_count: rendered_frame_counter,
                    focus_overlay_state: focus_overlay_state.clone(),
                    focus_overlay_area: focus_overlay_area.clone(),
                    focus_operation_label: focus_operation_label.clone(),
                    focus_method_label: focus_method_label.clone(),
                    mode_label: mode_label.clone(),
                    mode_dropdown: mode_dropdown.clone(),
                    iso_label: iso_label.clone(),
                    iso_dropdown: iso_dropdown.clone(),
                    shutter_speed_label: shutter_speed_label.clone(),
                    shutter_speed_dropdown: shutter_speed_dropdown.clone(),
                    current_shutter_speed_display: current_shutter_speed_display.clone(),
                    aperture_label: aperture_label.clone(),
                    aperture_dropdown: aperture_dropdown.clone(),
                    mode_dropdown_updating: mode_dropdown_updating.clone(),
                    iso_dropdown_updating: iso_dropdown_updating.clone(),
                    shutter_speed_dropdown_updating: shutter_speed_dropdown_updating.clone(),
                    aperture_dropdown_updating: aperture_dropdown_updating.clone(),
                    capture_settings_cache: capture_settings_cache.clone(),
                },
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
        let startup_logo = connected_view.startup_logo.clone();
        let live_view_picture = connected_view.live_view_picture.clone();
        let capture_button = connected_view.capture_button.clone();
        let capture_mode_switch = connected_view.capture_mode_switch.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        let focus_operation_label = connected_view.focus_operation_label.clone();
        let focus_method_label = connected_view.focus_method_label.clone();
        let mode_label = connected_view.mode_label.clone();
        let mode_dropdown = connected_view.mode_dropdown.clone();
        let iso_label = connected_view.iso_label.clone();
        let iso_dropdown = connected_view.iso_dropdown.clone();
        let shutter_speed_label = connected_view.shutter_speed_label.clone();
        let shutter_speed_dropdown = connected_view.shutter_speed_dropdown.clone();
        let aperture_label = connected_view.aperture_label.clone();
        let aperture_dropdown = connected_view.aperture_dropdown.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let rendered_frame_count = rendered_frame_count.clone();
        let capture_mode = capture_mode.clone();
        let video_recording = video_recording.clone();
        let connected = connected.clone();
        let startup_blink_source = startup_blink_source.clone();
        let mode_dropdown_updating = mode_dropdown_updating.clone();
        let iso_dropdown_updating = iso_dropdown_updating.clone();
        let shutter_speed_dropdown_updating = shutter_speed_dropdown_updating.clone();
        let aperture_dropdown_updating = aperture_dropdown_updating.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        disconnect_action.clone().connect_activate(move |_, _| {
            log_live_view("disconnect requested");
            connected.set(false);
            video_recording.set(false);
            *capture_settings_cache.borrow_mut() = None;
            let camera = configured_camera.borrow().clone();
            if let Some(session) = live_view_session.borrow_mut().take() {
                session.stop.store(true, Ordering::Relaxed);
                let session_cookie = session
                    .session_cookie
                    .lock()
                    .ok()
                    .and_then(|cookie| cookie.clone());
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
                if !camera.host.trim().is_empty() {
                    let base_url = format!("http://{}:{}", camera.host, camera.port);
                    if let Some(session_cookie) = session_cookie.as_deref() {
                        let _ = stop_live_view_transport(&base_url, session_cookie);
                    }
                    let _ = logout_browser_remote(&base_url, &camera, session_cookie.as_deref());
                }
            }
            rendered_frame_count.set(0);
            live_view_picture.set_paintable(Option::<&gtk::gdk::Texture>::None);
            stop_starting_animation(&startup_logo, &startup_blink_source);
            *focus_overlay_state.borrow_mut() = FocusOverlayState::default();
            focus_overlay_area.queue_draw();
            focus_operation_label.set_text("AF mode: -");
            focus_method_label.set_text("AF method: -");
            update_selectable_setting_dropdown(
                &mode_label,
                &mode_dropdown,
                &SelectableSettingState::default(),
                false,
                &mode_dropdown_updating,
                "shootingmode",
            );
            update_selectable_setting_dropdown(
                &iso_label,
                &iso_dropdown,
                &SelectableSettingState::default(),
                false,
                &iso_dropdown_updating,
                "iso",
            );
            update_selectable_setting_dropdown(
                &shutter_speed_label,
                &shutter_speed_dropdown,
                &SelectableSettingState::default(),
                false,
                &shutter_speed_dropdown_updating,
                "tv",
            );
            update_selectable_setting_dropdown(
                &aperture_label,
                &aperture_dropdown,
                &SelectableSettingState::default(),
                false,
                &aperture_dropdown_updating,
                "av",
            );
            update_connection_state(
                ContentViewState::Disconnected,
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
        let window = window.clone();
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
            let (cookie, pending_added_contents) = {
                let session = live_view_session.borrow();
                match session.as_ref() {
                    Some(session) => (
                        session
                            .session_cookie
                            .lock()
                            .ok()
                            .and_then(|cookie| cookie.clone()),
                        Some(session.pending_added_contents.clone()),
                    ),
                    None => (None, None),
                }
            };

            let Some(cookie) = cookie else {
                status_label.set_text(if capture_mode.get() == CaptureMode::Picture {
                    "Picture capture unavailable: no active camera session."
                } else {
                    "Video capture unavailable: no active camera session."
                });
                return;
            };
            let Some(pending_added_contents) = pending_added_contents else {
                status_label.set_text("Capture unavailable: no active camera session.");
                return;
            };

            match capture_mode.get() {
                CaptureMode::Picture => {
                    clear_pending_added_contents(&pending_added_contents);
                    log_live_view(format!(
                        "picture capture requested for {}:{}",
                        camera.host, camera.port
                    ));
                    status_label.set_text("Taking picture...");
                    flush_main_context();
                    match trigger_picture_capture(&camera, &cookie) {
                        Ok(()) => {
                            let storage_mode = storage.get();
                            disconnect_action.set_enabled(false);
                            status_label.set_text(match storage_mode {
                                StorageMode::CameraOnly => "Finalizing picture...",
                                _ => "Downloading...",
                            });
                            flush_main_context();
                            let result = apply_storage_policy_to_capture(
                                &camera,
                                &cookie,
                                &workspace.borrow(),
                                storage_mode,
                                CapturedMediaKind::Picture,
                                &pending_added_contents,
                            );
                            disconnect_action.set_enabled(connected.get());
                            match result {
                                Ok(outcome) => {
                                    status_label.set_text(&outcome.status_message);
                                    present_capture_result_dialog(
                                        &window,
                                        CapturedMediaKind::Picture,
                                        &outcome.path_labels,
                                    );
                                }
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
                        clear_pending_added_contents(&pending_added_contents);
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
                                disconnect_action.set_enabled(false);
                                status_label.set_text(match storage_mode {
                                    StorageMode::CameraOnly => "Finalizing video...",
                                    _ => "Downloading...",
                                });
                                flush_main_context();
                                let result = apply_storage_policy_to_capture(
                                    &camera,
                                    &cookie,
                                    &workspace.borrow(),
                                    storage_mode,
                                    CapturedMediaKind::Video,
                                    &pending_added_contents,
                                );
                                disconnect_action.set_enabled(connected.get());
                                match result {
                                    Ok(outcome) => {
                                        status_label.set_text(&outcome.status_message);
                                        present_capture_result_dialog(
                                            &window,
                                            CapturedMediaKind::Video,
                                            &outcome.path_labels,
                                        );
                                    }
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
                        clear_pending_added_contents(&pending_added_contents);
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
        let dropdown_updating = mode_dropdown_updating.clone();
        let capture_settings_controls = capture_settings_controls.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        connected_view
            .mode_dropdown
            .connect_selected_notify(move |dropdown| {
                if dropdown_updating.get() {
                    return;
                }

                let Some(value) = selected_dropdown_value(dropdown, "shootingmode") else {
                    return;
                };

                let camera = configured_camera.borrow().clone();
                let cookie = live_view_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| session.session_cookie.lock().ok()?.clone());
                let Some(cookie) = cookie else {
                    status_label.set_text("Mode unavailable: no active camera session.");
                    return;
                };

                apply_selectable_setting_change_async(
                    camera,
                    cookie,
                    "shootingmode",
                    value.clone(),
                    capture_settings_controls.clone(),
                    capture_settings_cache.clone(),
                    status_label.clone(),
                    format!(
                        "Mode set to {}.",
                        display_selectable_setting_value("shootingmode", &value)
                    ),
                    "Mode",
                );
            });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let dropdown_updating = iso_dropdown_updating.clone();
        let capture_settings_controls = capture_settings_controls.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        connected_view
            .iso_dropdown
            .connect_selected_notify(move |dropdown| {
                if dropdown_updating.get() {
                    return;
                }

                let Some(value) = selected_dropdown_value(dropdown, "iso") else {
                    return;
                };

                let camera = configured_camera.borrow().clone();
                let cookie = live_view_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| session.session_cookie.lock().ok()?.clone());
                let Some(cookie) = cookie else {
                    status_label.set_text("ISO unavailable: no active camera session.");
                    return;
                };

                apply_selectable_setting_change_async(
                    camera,
                    cookie,
                    "iso",
                    value.clone(),
                    capture_settings_controls.clone(),
                    capture_settings_cache.clone(),
                    status_label.clone(),
                    format!("ISO set to {value}."),
                    "ISO",
                );
            });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let dropdown_updating = shutter_speed_dropdown_updating.clone();
        let capture_settings_controls = capture_settings_controls.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        connected_view
            .shutter_speed_dropdown
            .connect_selected_notify(move |dropdown| {
                if dropdown_updating.get() {
                    return;
                }

                let Some(value) = selected_dropdown_value(dropdown, "tv") else {
                    return;
                };

                let camera = configured_camera.borrow().clone();
                let cookie = live_view_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| session.session_cookie.lock().ok()?.clone());
                let Some(cookie) = cookie else {
                    status_label.set_text("Shutter speed unavailable: no active camera session.");
                    return;
                };

                apply_selectable_setting_change_async(
                    camera,
                    cookie,
                    "tv",
                    value.clone(),
                    capture_settings_controls.clone(),
                    capture_settings_cache.clone(),
                    status_label.clone(),
                    format!("Shutter speed set to {value}."),
                    "Shutter speed",
                );
            });
    }

    {
        let status_label = status_label.clone();
        let live_view_session = live_view_session.clone();
        let configured_camera = configured_camera.clone();
        let dropdown_updating = aperture_dropdown_updating.clone();
        let capture_settings_controls = capture_settings_controls.clone();
        let capture_settings_cache = capture_settings_cache.clone();
        connected_view
            .aperture_dropdown
            .connect_selected_notify(move |dropdown| {
                if dropdown_updating.get() {
                    return;
                }

                let Some(value) = selected_dropdown_value(dropdown, "av") else {
                    return;
                };

                let camera = configured_camera.borrow().clone();
                let cookie = live_view_session
                    .borrow()
                    .as_ref()
                    .and_then(|session| session.session_cookie.lock().ok()?.clone());
                let Some(cookie) = cookie else {
                    status_label.set_text("Aperture unavailable: no active camera session.");
                    return;
                };

                apply_selectable_setting_change_async(
                    camera,
                    cookie,
                    "av",
                    value.clone(),
                    capture_settings_controls.clone(),
                    capture_settings_cache.clone(),
                    status_label.clone(),
                    format!("Aperture set to {value}."),
                    "Aperture",
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
        let status_label = status_label.clone();
        let configured_camera = configured_camera.clone();
        let live_view_session = live_view_session.clone();
        let focus_overlay_state = focus_overlay_state.clone();
        let focus_overlay_area = connected_view.focus_overlay_area.clone();
        let focus_overlay_area_for_handler = focus_overlay_area.clone();
        let focus_overlay_area_for_redraw = focus_overlay_area.clone();
        let click = GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_, n_press, x, y| {
            if n_press != 1 {
                return;
            }

            let camera = configured_camera.borrow().clone();
            let cookie = live_view_session
                .borrow()
                .as_ref()
                .and_then(|session| session.session_cookie.lock().ok()?.clone());

            let Some(cookie) = cookie else {
                status_label.set_text("Focus point unavailable: no active camera session.");
                return;
            };

            let new_state = {
                let current = focus_overlay_state.borrow().clone();
                match focus_overlay_state_from_click(
                    &current,
                    focus_overlay_area_for_handler.allocated_width() as f64,
                    focus_overlay_area_for_handler.allocated_height() as f64,
                    x,
                    y,
                ) {
                    Ok(state) => state,
                    Err(message) => {
                        status_label.set_text(message);
                        return;
                    }
                }
            };

            log_live_view(format!(
                "focus point click requested target=({}, {}) click=({x:.1}, {y:.1})",
                focus_target_x(&new_state),
                focus_target_y(&new_state)
            ));

            status_label.set_text("Moving focus point...");
            flush_main_context();

            match move_focus_point(&camera, &cookie, &new_state) {
                Ok(()) => {
                    *focus_overlay_state.borrow_mut() = new_state;
                    focus_overlay_area_for_redraw.queue_draw();
                }
                Err(error) => {
                    log_live_view(format!("focus point move failed: {error}"));
                    status_label.set_text(&format!("Focus point error: {error}"));
                    return;
                }
            }

            log_live_view(format!(
                "focus requested after click for {}:{}",
                camera.host, camera.port
            ));
            status_label.set_text("Focusing...");
            flush_main_context();

            match trigger_focus(&camera, &cookie) {
                Ok(()) => status_label.set_text("Focus complete."),
                Err(error) => {
                    log_live_view(format!("focus failed after click: {error}"));
                    status_label.set_text(&format!("Focus error: {error}"));
                }
            }
        });
        focus_overlay_area.add_controller(click);
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

    let album_menu = gio::Menu::new();
    album_menu.append(Some("Pictures"), Some("app.album-pictures"));
    album_menu.append(Some("Videos"), Some("app.album-videos"));
    left_root.append_submenu(Some("Album"), &album_menu);

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

    let starting = GtkBox::new(Orientation::Vertical, 12);
    starting.set_hexpand(true);
    starting.set_vexpand(true);
    starting.set_halign(Align::Center);
    starting.set_valign(Align::Center);

    let startup_logo = logo_picture(LOGO_256X256);
    startup_logo.set_halign(Align::Center);
    startup_logo.set_valign(Align::Center);
    let startup_label = Label::new(Some("Starting..."));
    startup_label.set_halign(Align::Center);
    starting.append(&startup_logo);
    starting.append(&startup_label);

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

    let exposure_title = Label::new(Some("Exposure"));
    exposure_title.set_halign(Align::Start);
    exposure_title.add_css_class("heading");

    let settings_grid = Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .hexpand(true)
        .build();
    let mode_label = Label::builder().label("Mode").halign(Align::End).build();
    let iso_label = Label::builder().label("ISO").halign(Align::End).build();
    let shutter_speed_label = Label::builder()
        .label("Shutter speed")
        .halign(Align::End)
        .build();
    let aperture_label = Label::builder()
        .label("Aperture")
        .halign(Align::End)
        .build();
    let mode_dropdown = DropDown::from_strings(&["-"]);
    let iso_dropdown = DropDown::from_strings(&["-"]);
    let shutter_speed_dropdown = DropDown::from_strings(&["-"]);
    let aperture_dropdown = DropDown::from_strings(&["-"]);
    mode_dropdown.set_sensitive(false);
    iso_dropdown.set_sensitive(false);
    shutter_speed_dropdown.set_sensitive(false);
    aperture_dropdown.set_sensitive(false);
    let current_shutter_speed_display = Label::new(None);
    current_shutter_speed_display.set_hexpand(true);
    current_shutter_speed_display.set_halign(Align::Fill);
    current_shutter_speed_display.set_justify(gtk::Justification::Center);
    current_shutter_speed_display.set_xalign(0.5);
    current_shutter_speed_display.add_css_class("title-1");
    let shutter_speed_display_spacer = GtkBox::new(Orientation::Vertical, 0);
    shutter_speed_display_spacer.set_vexpand(true);
    settings_grid.attach(&mode_label, 0, 0, 1, 1);
    settings_grid.attach(&mode_dropdown, 1, 0, 1, 1);
    settings_grid.attach(&iso_label, 0, 1, 1, 1);
    settings_grid.attach(&iso_dropdown, 1, 1, 1, 1);
    settings_grid.attach(&shutter_speed_label, 0, 2, 1, 1);
    settings_grid.attach(&shutter_speed_dropdown, 1, 2, 1, 1);
    settings_grid.attach(&aperture_label, 0, 3, 1, 1);
    settings_grid.attach(&aperture_dropdown, 1, 3, 1, 1);

    side_panel.append(&capture_title);
    side_panel.append(&capture_mode_row);
    side_panel.append(&capture_button);
    side_panel.append(&panel_title);
    side_panel.append(&focus_operation_label);
    side_panel.append(&focus_method_label);
    side_panel.append(&focus_grid);
    side_panel.append(&exposure_title);
    side_panel.append(&settings_grid);
    side_panel.append(&shutter_speed_display_spacer);
    side_panel.append(&current_shutter_speed_display);

    connected.append(&live_view_overlay);
    connected.append(&side_panel);

    stack.add_named(&disconnected, Some("disconnected"));
    stack.add_named(&starting, Some("starting"));
    stack.add_named(&connected, Some("connected"));
    content.append(&stack);
    ConnectedView {
        content,
        content_stack: stack,
        startup_logo,
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
        mode_label,
        mode_dropdown,
        iso_label,
        iso_dropdown,
        shutter_speed_label,
        shutter_speed_dropdown,
        current_shutter_speed_display,
        aperture_label,
        aperture_dropdown,
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
    dialog.set_default_size(680, 420);
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
    let scan_mask_entry = Entry::builder()
        .text(default_camera_scan_mask(&current.host))
        .placeholder_text("192.168.1.xxx")
        .hexpand(true)
        .build();
    let scan_button = Button::with_label("Scan");
    let scan_controls = GtkBox::new(Orientation::Horizontal, 6);
    scan_controls.append(&scan_mask_entry);
    scan_controls.append(&scan_button);
    let scanned_camera_dropdown = DropDown::from_strings(&["No scanned Canon cameras"]);
    scanned_camera_dropdown.set_sensitive(false);
    let scan_status_label = Label::new(Some(
        "Enter a network mask like 192.168.1.xxx to scan for Canon cameras.",
    ));
    scan_status_label.set_halign(Align::Start);
    scan_status_label.set_wrap(true);
    let scanned_cameras = Rc::new(RefCell::new(Vec::<NetworkCamera>::new()));

    attach_form_row(&grid, 0, "Camera", &camera_name_entry);
    attach_form_row(&grid, 1, "Host", &host_entry);
    attach_form_row(&grid, 2, "Port", &port_spin);
    attach_form_row(&grid, 3, "Username", &username_entry);
    attach_form_row(&grid, 4, "Password", &password_entry);
    attach_form_row(&grid, 5, "Scan mask", &scan_controls);
    attach_form_row(&grid, 6, "Detected", &scanned_camera_dropdown);
    grid.attach(&scan_status_label, 1, 7, 1, 1);

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

    {
        let scanned_cameras = scanned_cameras.clone();
        let camera_name_entry = camera_name_entry.clone();
        let host_entry = host_entry.clone();
        let port_spin = port_spin.clone();
        scanned_camera_dropdown.connect_selected_notify(move |dropdown| {
            let selected_index = dropdown.selected() as usize;
            let selected_camera = scanned_cameras.borrow().get(selected_index).cloned();
            if let Some(camera) = selected_camera {
                camera_name_entry.set_text(&network::suggest_camera_name(Some(&camera)));
                host_entry.set_text(&camera.address);
                port_spin.set_value(camera.port as f64);
            }
        });
    }

    {
        let scan_button = scan_button.clone();
        let scan_mask_entry = scan_mask_entry.clone();
        let scanned_camera_dropdown = scanned_camera_dropdown.clone();
        let scan_status_label = scan_status_label.clone();
        let scanned_cameras = scanned_cameras.clone();
        let host_entry = host_entry.clone();
        scan_button.clone().connect_clicked(move |_| {
            let mask = scan_mask_entry.text().trim().to_owned();
            if mask.is_empty() {
                scan_status_label.set_text("Enter a network mask like 192.168.1.xxx.");
                return;
            }

            scan_button.set_sensitive(false);
            scanned_camera_dropdown.set_sensitive(false);
            scan_status_label.set_text(&format!("Scanning {mask} for Canon cameras..."));
            flush_main_context();

            let (sender, receiver) = mpsc::channel::<Result<Vec<NetworkCamera>, String>>();
            let worker_mask = mask.clone();
            thread::spawn(move || {
                let result = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("Failed to start scan worker: {error}"))
                    .and_then(|runtime| {
                        runtime.block_on(network::discover_canon_cameras_in_mask(&worker_mask))
                    });
                let _ = sender.send(result);
            });

            let scan_button = scan_button.clone();
            let scanned_camera_dropdown = scanned_camera_dropdown.clone();
            let scan_status_label = scan_status_label.clone();
            let scanned_cameras = scanned_cameras.clone();
            let host_entry = host_entry.clone();
            let _ = glib::timeout_add_local(Duration::from_millis(33), move || {
                match receiver.try_recv() {
                    Ok(Ok(cameras)) => {
                        let preferred_host = host_entry.text().trim().to_owned();
                        scan_button.set_sensitive(true);
                        *scanned_cameras.borrow_mut() = cameras;
                        update_scanned_camera_dropdown(
                            &scanned_camera_dropdown,
                            &scanned_cameras.borrow(),
                            &preferred_host,
                        );

                        let camera_count = scanned_cameras.borrow().len();
                        if camera_count == 0 {
                            scan_status_label
                                .set_text(&format!("No Canon cameras found for {mask}."));
                        } else {
                            let plural = if camera_count == 1 { "" } else { "s" };
                            scan_status_label.set_text(&format!(
                                "Found {camera_count} Canon camera{plural} in {mask}."
                            ));
                        }

                        ControlFlow::Break
                    }
                    Ok(Err(error)) => {
                        scan_button.set_sensitive(true);
                        scanned_cameras.borrow_mut().clear();
                        update_scanned_camera_dropdown(
                            &scanned_camera_dropdown,
                            &[],
                            host_entry.text().trim(),
                        );
                        scan_status_label.set_text(&format!("Scan failed: {error}"));
                        ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        scan_button.set_sensitive(true);
                        scan_status_label.set_text("Scan failed: scan worker stopped.");
                        ControlFlow::Break
                    }
                }
            });
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

fn present_capture_result_dialog(
    parent: &ApplicationWindow,
    media_kind: CapturedMediaKind,
    path_labels: &[String],
) {
    if path_labels.is_empty() {
        return;
    }

    let singular = path_labels.len() == 1;
    let noun = match media_kind {
        CapturedMediaKind::Picture => {
            if singular {
                "Picture path"
            } else {
                "Picture paths"
            }
        }
        CapturedMediaKind::Video => {
            if singular {
                "Video path"
            } else {
                "Video paths"
            }
        }
    };

    let dialog = Dialog::builder()
        .title(noun)
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();
    dialog.add_button("Close", ResponseType::Close);

    let content_area = dialog.content_area();
    content_area.set_spacing(12);
    content_area.set_margin_top(16);
    content_area.set_margin_bottom(16);
    content_area.set_margin_start(24);
    content_area.set_margin_end(24);

    let summary = Label::new(Some("Capture completed:"));
    summary.set_halign(Align::Start);

    let paths_label = Label::new(Some(&path_labels.join("\n")));
    paths_label.set_halign(Align::Start);
    paths_label.set_xalign(0.0);
    paths_label.set_wrap(true);
    paths_label.set_selectable(true);

    content_area.append(&summary);
    content_area.append(&paths_label);

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
    dialog.present();
}

fn present_album_dialog(
    parent: &ApplicationWindow,
    media_kind: CapturedMediaKind,
    workspace: &Path,
    status_label: &Label,
) {
    let media_paths = match collect_workspace_media(workspace, media_kind) {
        Ok(paths) => paths,
        Err(error) => {
            status_label.set_text(&format!("Failed to load album: {error}"));
            return;
        }
    };

    let dialog = Dialog::builder()
        .title(album_dialog_title(media_kind))
        .transient_for(parent)
        .modal(true)
        .default_width(720)
        .default_height(420)
        .build();
    dialog.add_button("Close", ResponseType::Close);

    let content_area = dialog.content_area();
    content_area.set_spacing(12);
    content_area.set_margin_top(16);
    content_area.set_margin_bottom(16);
    content_area.set_margin_start(24);
    content_area.set_margin_end(24);

    if media_paths.is_empty() {
        let empty_label = Label::new(Some("No captured media found in the workspace."));
        empty_label.set_halign(Align::Start);
        empty_label.set_wrap(true);
        content_area.append(&empty_label);
    } else {
        let scroller = ScrolledWindow::builder()
            .min_content_width(640)
            .min_content_height(320)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        let list = ListBox::new();
        list.set_activate_on_single_click(false);

        let paths = Rc::new(RefCell::new(media_paths));
        for path in paths.borrow().iter() {
            let row = gtk::ListBoxRow::new();
            let row_content = GtkBox::new(Orientation::Horizontal, 12);
            row_content.set_margin_top(8);
            row_content.set_margin_bottom(8);
            row_content.set_margin_start(12);
            row_content.set_margin_end(12);
            row_content.set_hexpand(true);

            let text_column = GtkBox::new(Orientation::Vertical, 4);
            text_column.set_hexpand(true);

            let title = Label::new(Some(&media_display_name(path)));
            title.set_halign(Align::Start);
            title.set_xalign(0.0);
            title.set_hexpand(true);

            let full_path = Label::new(Some(&path.display().to_string()));
            full_path.set_halign(Align::Start);
            full_path.set_xalign(0.0);
            full_path.set_wrap(true);
            full_path.add_css_class("dim-label");
            full_path.set_hexpand(true);

            text_column.append(&title);
            text_column.append(&full_path);
            row_content.append(&text_column);

            let buttons = GtkBox::new(Orientation::Horizontal, 6);
            let view_button = Button::builder()
                .icon_name("document-open-symbolic")
                .tooltip_text("View")
                .build();
            let delete_button = Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete")
                .build();
            buttons.append(&view_button);
            buttons.append(&delete_button);
            row_content.append(&buttons);

            {
                let status_label = status_label.clone();
                let path = path.clone();
                view_button.connect_clicked(move |_| {
                    open_album_media_path(&path, &status_label);
                });
            }

            {
                let status_label = status_label.clone();
                let path = path.clone();
                let paths = paths.clone();
                let list = list.clone();
                let row = row.clone();
                delete_button.connect_clicked(move |_| match fs::remove_file(&path) {
                    Ok(()) => {
                        status_label.set_text(&format!("Deleted {}", path.display()));
                        let index = row.index();
                        list.remove(&row);
                        if let Ok(index) = usize::try_from(index) {
                            let mut paths = paths.borrow_mut();
                            if index < paths.len() {
                                paths.remove(index);
                            }
                        }
                    }
                    Err(error) => {
                        status_label
                            .set_text(&format!("Failed to delete {}: {error}", path.display()));
                    }
                });
            }

            row.set_child(Some(&row_content));
            list.append(&row);
        }

        let status_label = status_label.clone();
        let paths_for_activation = paths.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index();
            let Some(path) = usize::try_from(index)
                .ok()
                .and_then(|index| paths_for_activation.borrow().get(index).cloned())
            else {
                status_label.set_text("Unable to open the selected media path.");
                return;
            };

            open_album_media_path(&path, &status_label);
        });

        scroller.set_child(Some(&list));
        content_area.append(&scroller);
    }

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
    dialog.present();
}

fn album_dialog_title(media_kind: CapturedMediaKind) -> &'static str {
    match media_kind {
        CapturedMediaKind::Picture => "Pictures",
        CapturedMediaKind::Video => "Videos",
    }
}

fn collect_workspace_media(
    workspace: &Path,
    media_kind: CapturedMediaKind,
) -> Result<Vec<PathBuf>, String> {
    if !workspace.exists() {
        return Ok(Vec::new());
    }

    let mut stack = vec![workspace.to_path_buf()];
    let mut media_paths = Vec::new();

    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;

        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to read directory entry in {}: {error}",
                    directory.display()
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if workspace_media_matches_kind(&path, media_kind) {
                media_paths.push(path);
            }
        }
    }

    media_paths.sort();
    media_paths.reverse();
    Ok(media_paths)
}

fn media_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn open_album_media_path(path: &Path, status_label: &Label) {
    let absolute_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            status_label.set_text(&format!("Failed to resolve {}: {error}", path.display()));
            return;
        }
    };

    if workspace_media_matches_kind(&absolute_path, CapturedMediaKind::Video) {
        match open_video_media_path(&absolute_path) {
            Ok(launcher_name) => {
                status_label.set_text(&format!(
                    "Opened {} via {launcher_name}",
                    absolute_path.display()
                ));
                return;
            }
            Err(video_error) => {
                log_live_view(format!(
                    "video player launch failed for {}: {video_error}",
                    absolute_path.display()
                ));
            }
        }
    }

    let file = gio::File::for_path(&absolute_path);
    let uri = file.uri();

    match gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
        Ok(()) => status_label.set_text(&format!("Opened {}", absolute_path.display())),
        Err(gio_error) => match opener::open(&absolute_path) {
            Ok(()) => status_label.set_text(&format!("Opened {}", absolute_path.display())),
            Err(opener_error) => match open_media_path_with_desktop_command(&absolute_path) {
                Ok(launcher_name) => status_label.set_text(&format!(
                    "Opened {} via {launcher_name}",
                    absolute_path.display()
                )),
                Err(command_error) => {
                    status_label.set_text(&format!(
                        "Failed to open {}: {gio_error}; opener fallback failed: {opener_error}; desktop launcher fallback failed: {command_error}",
                        absolute_path.display()
                    ));
                }
            },
        },
    }
}

fn open_video_media_path(path: &Path) -> Result<&'static str, String> {
    for (launcher_name, command, extra_args) in [
        ("mpv", "mpv", &[][..]),
        ("vlc", "vlc", &[][..]),
        ("totem", "totem", &[][..]),
        ("mplayer", "mplayer", &[][..]),
        ("ffplay", "ffplay", &["-autoexit"][..]),
    ] {
        let mut child = Command::new(command);
        child
            .args(extra_args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match child.spawn() {
            Ok(_) => return Ok(launcher_name),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                log_live_view(format!(
                    "{launcher_name} unavailable for {}: {error}",
                    path.display()
                ));
            }
            Err(error) => {
                log_live_view(format!(
                    "{launcher_name} failed for {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Err("no supported video player command succeeded".to_owned())
}

fn open_media_path_with_desktop_command(path: &Path) -> Result<&'static str, String> {
    for (launcher_name, command) in [
        (
            "gio open",
            ("gio", vec!["open".to_owned(), path.display().to_string()]),
        ),
        ("xdg-open", ("xdg-open", vec![path.display().to_string()])),
    ] {
        match Command::new(command.0).args(&command.1).status() {
            Ok(status) if status.success() => return Ok(launcher_name),
            Ok(status) => {
                let code = status.code().map_or_else(
                    || "terminated by signal".to_owned(),
                    |code| code.to_string(),
                );
                log_live_view(format!(
                    "{launcher_name} failed for {} with status {code}",
                    path.display()
                ));
            }
            Err(error) => {
                log_live_view(format!(
                    "{launcher_name} unavailable for {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Err("no desktop launcher succeeded".to_owned())
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

fn default_camera_scan_mask(host: &str) -> String {
    let mut octets = host.trim().split('.');
    let (Some(first), Some(second), Some(third), Some(fourth)) =
        (octets.next(), octets.next(), octets.next(), octets.next())
    else {
        return String::new();
    };

    if octets.next().is_some() {
        return String::new();
    }

    for octet in [first, second, third, fourth] {
        if octet.parse::<u8>().is_err() {
            return String::new();
        }
    }

    format!("{first}.{second}.{third}.xxx")
}

fn update_scanned_camera_dropdown(
    dropdown: &DropDown,
    cameras: &[NetworkCamera],
    preferred_host: &str,
) {
    let model = StringList::new(&[]);

    if cameras.is_empty() {
        model.append("No scanned Canon cameras");
        dropdown.set_model(Some(&model));
        dropdown.set_selected(0);
        dropdown.set_sensitive(false);
        return;
    }

    for camera in cameras {
        model.append(&format!("{} ({})", camera.display_name(), camera.address));
    }

    dropdown.set_model(Some(&model));
    let selected_index = cameras
        .iter()
        .position(|camera| camera.address == preferred_host)
        .unwrap_or(0) as u32;
    dropdown.set_selected(selected_index);
    dropdown.set_sensitive(true);
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
    state: ContentViewState,
    status_label: &Label,
    connect_action: &gio::SimpleAction,
    disconnect_action: &gio::SimpleAction,
    capture_action: &gio::SimpleAction,
    focus_action: &gio::SimpleAction,
    content_stack: &Stack,
) {
    match state {
        ContentViewState::Disconnected => {
            status_label.set_text("Camera disconnected.");
            connect_action.set_enabled(true);
            disconnect_action.set_enabled(false);
            capture_action.set_enabled(false);
            focus_action.set_enabled(false);
            content_stack.set_visible_child_name("disconnected");
        }
        ContentViewState::Starting => {
            status_label.set_text("Starting...");
            connect_action.set_enabled(false);
            disconnect_action.set_enabled(false);
            capture_action.set_enabled(false);
            focus_action.set_enabled(false);
            content_stack.set_visible_child_name("starting");
        }
        ContentViewState::Connected => {
            status_label.set_text("Camera connected.");
            connect_action.set_enabled(false);
            disconnect_action.set_enabled(true);
            capture_action.set_enabled(true);
            focus_action.set_enabled(true);
            content_stack.set_visible_child_name("connected");
        }
    }
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

fn update_selectable_setting_dropdown(
    label: &Label,
    dropdown: &DropDown,
    state: &SelectableSettingState,
    connected: bool,
    updating: &Cell<bool>,
    setting_name: &str,
) {
    updating.set(true);

    let mut raw_values = if state.ability.is_empty() {
        if state.current.is_empty() {
            vec!["-".to_owned()]
        } else {
            vec![state.current.clone()]
        }
    } else {
        state.ability.clone()
    };

    if !state.current.is_empty() && !raw_values.iter().any(|value| value == &state.current) {
        raw_values.insert(0, state.current.clone());
    }

    let model = StringList::new(&[]);
    for value in &raw_values {
        model.append(&display_selectable_setting_value(setting_name, value));
    }
    dropdown.set_model(Some(&model));

    let selected_index = raw_values
        .iter()
        .position(|value| value == &state.current)
        .unwrap_or(0) as u32;
    dropdown.set_selected(selected_index);
    let enabled = connected && state.is_available();
    label.set_sensitive(enabled);
    dropdown.set_sensitive(enabled);

    updating.set(false);
}

fn selected_dropdown_value(dropdown: &DropDown, setting_name: &str) -> Option<String> {
    dropdown
        .selected_item()
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .map(|item| internal_selectable_setting_value(setting_name, &item.string()))
        .filter(|value| value != "-")
}

fn display_selectable_setting_value(setting_name: &str, value: &str) -> String {
    if setting_name == "shootingmode" {
        match value {
            "fv" => "Flexible priority (Fv)".to_owned(),
            "p" => "Program AE (P)".to_owned(),
            "av" => "Aperture priority (Av)".to_owned(),
            "tv" => "Shutter priority (Tv)".to_owned(),
            "m" => "Manual (M)".to_owned(),
            "bulb" => "Bulb".to_owned(),
            "c1" => "Custom 1 (C1)".to_owned(),
            "c2" => "Custom 2 (C2)".to_owned(),
            "c3" => "Custom 3 (C3)".to_owned(),
            "auto" => "Auto".to_owned(),
            other => other.to_owned(),
        }
    } else {
        value.to_owned()
    }
}

fn internal_selectable_setting_value(setting_name: &str, value: &str) -> String {
    if setting_name == "shootingmode" {
        match value {
            "Flexible priority (Fv)" => "fv".to_owned(),
            "Program AE (P)" => "p".to_owned(),
            "Aperture priority (Av)" => "av".to_owned(),
            "Shutter priority (Tv)" => "tv".to_owned(),
            "Manual (M)" => "m".to_owned(),
            "Bulb" => "bulb".to_owned(),
            "Custom 1 (C1)" => "c1".to_owned(),
            "Custom 2 (C2)" => "c2".to_owned(),
            "Custom 3 (C3)" => "c3".to_owned(),
            "Auto" => "auto".to_owned(),
            other => other.to_owned(),
        }
    } else {
        value.to_owned()
    }
}

fn update_capture_settings_controls(
    controls: &CaptureSettingsControls,
    state: &CaptureSettingsState,
    connected: bool,
) {
    update_selectable_setting_dropdown(
        &controls.mode_label,
        &controls.mode_dropdown,
        &state.mode,
        connected,
        &controls.mode_dropdown_updating,
        "shootingmode",
    );
    update_selectable_setting_dropdown(
        &controls.iso_label,
        &controls.iso_dropdown,
        &state.iso,
        connected,
        &controls.iso_dropdown_updating,
        "iso",
    );
    update_selectable_setting_dropdown(
        &controls.shutter_speed_label,
        &controls.shutter_speed_dropdown,
        &state.shutter_speed,
        connected,
        &controls.shutter_speed_dropdown_updating,
        "tv",
    );
    update_selectable_setting_dropdown(
        &controls.aperture_label,
        &controls.aperture_dropdown,
        &state.aperture,
        connected,
        &controls.aperture_dropdown_updating,
        "av",
    );
    update_current_shutter_speed_display(
        &controls.current_shutter_speed_display,
        &state.display_exposure(),
    );
}

fn set_capture_settings_controls_sensitive(controls: &CaptureSettingsControls, sensitive: bool) {
    controls.mode_label.set_sensitive(sensitive);
    controls.mode_dropdown.set_sensitive(sensitive);
    controls.iso_label.set_sensitive(sensitive);
    controls.iso_dropdown.set_sensitive(sensitive);
    controls.shutter_speed_label.set_sensitive(sensitive);
    controls.shutter_speed_dropdown.set_sensitive(sensitive);
    controls
        .current_shutter_speed_display
        .set_sensitive(sensitive);
    controls.aperture_label.set_sensitive(sensitive);
    controls.aperture_dropdown.set_sensitive(sensitive);
}

fn update_current_shutter_speed_display(display: &Label, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        display.set_text(value);
    }
}

fn apply_cached_selectable_setting_change(
    capture_settings_cache: &Rc<RefCell<Option<CaptureSettingsCache>>>,
    setting_name: &str,
    value: &str,
) -> Option<CaptureSettingsState> {
    let mut cache_ref = capture_settings_cache.borrow_mut();
    let cache = cache_ref.as_mut()?;

    if setting_name == "shootingmode" {
        cache.current_mode = value.to_owned();
        return cache.by_mode.get(value).cloned();
    }

    let current_mode = cache.current_mode.clone();
    let state = cache.by_mode.get_mut(&current_mode)?;
    match setting_name {
        "iso" => state.iso.current = value.to_owned(),
        "tv" => state.shutter_speed.current = value.to_owned(),
        "av" => state.aperture.current = value.to_owned(),
        _ => return None,
    }
    Some(state.clone())
}

fn apply_refreshed_capture_settings_state(
    capture_settings_cache: &Rc<RefCell<Option<CaptureSettingsCache>>>,
    state: &CaptureSettingsState,
) {
    let mut cache_ref = capture_settings_cache.borrow_mut();
    let Some(cache) = cache_ref.as_mut() else {
        return;
    };

    if !state.mode.current.is_empty() {
        cache.current_mode = state.mode.current.clone();
        cache
            .by_mode
            .insert(state.mode.current.clone(), state.clone());
    }
}

fn apply_selectable_setting_change_async(
    camera: ConfiguredCamera,
    session_cookie: String,
    setting_name: &'static str,
    value: String,
    controls: CaptureSettingsControls,
    capture_settings_cache: Rc<RefCell<Option<CaptureSettingsCache>>>,
    status_label: Label,
    success_message: String,
    status_prefix: &'static str,
) {
    set_capture_settings_controls_sensitive(&controls, false);
    status_label.set_text(&format!("Updating {status_prefix}..."));
    flush_main_context();

    let controls = controls.clone();
    let capture_settings_cache = capture_settings_cache.clone();
    let status_label = status_label.clone();
    let worker_value = value.clone();
    let (sender, receiver) = mpsc::channel::<(Result<(), String>, Option<CaptureSettingsState>)>();

    thread::spawn(move || {
        let update_result =
            update_selectable_camera_setting(&camera, &session_cookie, setting_name, &worker_value);
        let refreshed_state = if update_result.is_ok() {
            let state = refresh_capture_settings_state_after_change(&camera, &session_cookie).ok();
            // The camera stops rendering live view a moment after applying
            // some setting changes (the stream keeps delivering blank frames);
            // turn it back on once the change has settled.
            let base_url = format!("http://{}:{}", camera.host, camera.port);
            match enable_live_view(&base_url, &session_cookie) {
                Ok(LiveViewEnableOutcome::Enabled | LiveViewEnableOutcome::Busy) => {}
                Err(error) => log_live_view(format!("live view enable failed: {error}")),
            }
            state
        } else {
            None
        };
        let _ = sender.send((update_result, refreshed_state));
    });

    let _ = glib::timeout_add_local(Duration::from_millis(33), move || {
        match receiver.try_recv() {
            Ok((result, refreshed_state)) => {
                match result {
                    Ok(()) => {
                        let state = if let Some(refreshed_state) = refreshed_state {
                            apply_refreshed_capture_settings_state(
                                &capture_settings_cache,
                                &refreshed_state,
                            );
                            Some(refreshed_state)
                        } else {
                            apply_cached_selectable_setting_change(
                                &capture_settings_cache,
                                setting_name,
                                &value,
                            )
                        };

                        if let Some(state) = state {
                            update_capture_settings_controls(&controls, &state, true);
                        } else {
                            set_capture_settings_controls_sensitive(&controls, true);
                        }
                        status_label.set_text(&success_message);
                    }
                    Err(error) => {
                        set_capture_settings_controls_sensitive(&controls, true);
                        log_live_view(format!("{setting_name} update failed: {error}"));
                        status_label.set_text(&format!("{status_prefix} error: {error}"));
                    }
                }
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                set_capture_settings_controls_sensitive(&controls, true);
                status_label.set_text(&format!("{status_prefix} error: update worker stopped."));
                ControlFlow::Break
            }
        }
    });
}

fn fetch_capture_settings_state(
    camera: &ConfiguredCamera,
    session_cookie: &str,
) -> Result<CaptureSettingsState, String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "GET",
        &format!("{base_url}/brapi/currentproperty"),
        Some(session_cookie),
        Some(&referer),
        None,
    )?;
    log_live_view(format!(
        "capture settings refresh status={status} body_prefix={}",
        preview_text(&body)
    ));

    if status != 200 {
        return Err(format!(
            "capture settings refresh failed with {status}: {body}"
        ));
    }

    parse_capture_settings_state(&body)
        .ok_or_else(|| "capture settings refresh returned no selectable settings".to_owned())
}

fn enable_live_view(base_url: &str, session_cookie: &str) -> Result<LiveViewEnableOutcome, String> {
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "POST",
        &format!("{base_url}/ccapi/ver100/shooting/liveview"),
        Some(session_cookie),
        Some(&referer),
        Some(r#"{"liveviewsize":"medium","cameradisplay":"on"}"#),
    )?;
    log_live_view(format!(
        "live view enable status={status} body_prefix={}",
        preview_text(&body)
    ));

    if matches!(status, 200 | 204) {
        Ok(LiveViewEnableOutcome::Enabled)
    } else if status == 503 && error_indicates_camera_busy(&body) {
        Ok(LiveViewEnableOutcome::Busy)
    } else {
        Err(format!("live view enable failed with {status}: {body}"))
    }
}

fn stop_live_view_transport(base_url: &str, session_cookie: &str) -> Result<(), String> {
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let (status, body) = run_curl_request(
        "DELETE",
        &format!("{base_url}/brapi/shooting/lvscrolldetail?liveviewsize=off"),
        Some(session_cookie),
        Some(&referer),
        None,
    )?;
    log_live_view(format!(
        "live view transport stop status={status} body_prefix={}",
        preview_text(&body)
    ));

    if matches!(status, 200 | 204) || (status == 503 && body.contains("Already stopped")) {
        Ok(())
    } else {
        Err(format!(
            "live view transport stop failed with {status}: {body}"
        ))
    }
}

fn refresh_capture_settings_state_after_change(
    camera: &ConfiguredCamera,
    session_cookie: &str,
) -> Result<CaptureSettingsState, String> {
    const SETTLE_DELAY: Duration = Duration::from_millis(300);

    thread::sleep(SETTLE_DELAY);
    fetch_metered_capture_settings_state(camera, session_cookie)
}

fn fetch_metered_capture_settings_state(
    camera: &ConfiguredCamera,
    session_cookie: &str,
) -> Result<CaptureSettingsState, String> {
    const REFETCH_ATTEMPTS: usize = 3;
    const REFETCH_DELAY: Duration = Duration::from_millis(300);

    // User-selected values are reported by the fetch directly; the camera
    // only publishes the estimated ones (`effective_value_tv` in P/Av/auto,
    // `effective_value_av` in P/Tv/auto) after it recalculates the exposure,
    // which an AF cycle reliably triggers.
    let state = fetch_capture_settings_state(camera, session_cookie)?;
    if state.has_complete_exposure() {
        return Ok(state);
    }

    if let Err(error) = trigger_focus(camera, session_cookie) {
        log_live_view(format!("metering AF cycle failed: {error}"));
        return Ok(state);
    }

    let mut state = state;
    for _ in 0..REFETCH_ATTEMPTS {
        thread::sleep(REFETCH_DELAY);
        state = fetch_capture_settings_state(camera, session_cookie)?;
        if state.has_complete_exposure() {
            break;
        }
    }
    log_live_view(format!(
        "metered capture settings mode={} tv={} effective_tv={} av={} effective_av={}",
        state.mode.current,
        state.shutter_speed.current,
        state.effective_shutter_speed,
        state.aperture.current,
        state.effective_aperture
    ));
    Ok(state)
}

fn build_capture_settings_cache(
    initial_state: CaptureSettingsState,
) -> (CaptureSettingsCache, CaptureSettingsState) {
    let mut by_mode = HashMap::new();
    if !initial_state.mode.current.is_empty() {
        by_mode.insert(initial_state.mode.current.clone(), initial_state.clone());
    }

    (
        CaptureSettingsCache {
            current_mode: initial_state.mode.current.clone(),
            by_mode,
        },
        initial_state,
    )
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

fn start_starting_animation(logo: &Picture, blink_source: &Rc<RefCell<Option<SourceId>>>) {
    stop_starting_animation(logo, blink_source);

    let logo = logo.clone();
    let mut dimmed = false;
    let source = glib::timeout_add_local(Duration::from_millis(500), move || {
        dimmed = !dimmed;
        logo.set_opacity(if dimmed { 0.35 } else { 1.0 });
        ControlFlow::Continue
    });
    *blink_source.borrow_mut() = Some(source);
}

fn stop_starting_animation(logo: &Picture, blink_source: &Rc<RefCell<Option<SourceId>>>) {
    if let Some(source) = blink_source.borrow_mut().take() {
        source.remove();
    }
    logo.set_opacity(1.0);
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
    ui: LiveViewUiBindings,
) -> LiveViewSession {
    let LiveViewUiBindings {
        live_view_picture,
        status_label,
        connect_action,
        disconnect_action,
        capture_action,
        focus_action,
        content_stack,
        startup_logo,
        startup_blink_source,
        rendered_frame_count,
        focus_overlay_state,
        focus_overlay_area,
        focus_operation_label,
        focus_method_label,
        mode_label,
        mode_dropdown,
        iso_label,
        iso_dropdown,
        shutter_speed_label,
        shutter_speed_dropdown,
        current_shutter_speed_display,
        aperture_label,
        aperture_dropdown,
        mode_dropdown_updating,
        iso_dropdown_updating,
        shutter_speed_dropdown_updating,
        aperture_dropdown_updating,
        capture_settings_cache,
    } = ui;
    let mut startup_complete = false;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = stop.clone();
    let stop_ui = stop.clone();
    let child_pid = Arc::new(Mutex::new(None));
    let child_pid_worker = child_pid.clone();
    let session_cookie = Arc::new(Mutex::new(None));
    let session_cookie_worker = session_cookie.clone();
    let pending_added_contents = Arc::new(Mutex::new(Vec::new()));
    let pending_added_contents_worker = pending_added_contents.clone();
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
                        if !startup_complete {
                            startup_complete = true;
                            stop_starting_animation(&startup_logo, &startup_blink_source);
                            update_connection_state(
                                ContentViewState::Connected,
                                &status_label,
                                &connect_action,
                                &disconnect_action,
                                &capture_action,
                                &focus_action,
                                &content_stack,
                            );
                        }
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
                LiveViewEvent::CaptureSettings(state) => {
                    update_selectable_setting_dropdown(
                        &mode_label,
                        &mode_dropdown,
                        &state.mode,
                        true,
                        &mode_dropdown_updating,
                        "shootingmode",
                    );
                    update_selectable_setting_dropdown(
                        &iso_label,
                        &iso_dropdown,
                        &state.iso,
                        true,
                        &iso_dropdown_updating,
                        "iso",
                    );
                    update_selectable_setting_dropdown(
                        &shutter_speed_label,
                        &shutter_speed_dropdown,
                        &state.shutter_speed,
                        true,
                        &shutter_speed_dropdown_updating,
                        "tv",
                    );
                    update_selectable_setting_dropdown(
                        &aperture_label,
                        &aperture_dropdown,
                        &state.aperture,
                        true,
                        &aperture_dropdown_updating,
                        "av",
                    );
                    update_current_shutter_speed_display(
                        &current_shutter_speed_display,
                        &state.display_exposure(),
                    );
                }
                LiveViewEvent::CaptureSettingsCache(cache) => {
                    *capture_settings_cache.borrow_mut() = Some(cache);
                }
                LiveViewEvent::EffectiveExposure {
                    shutter_speed,
                    aperture,
                } => {
                    let mut cache_ref = capture_settings_cache.borrow_mut();
                    let current_state = cache_ref.as_mut().and_then(|cache| {
                        let current_mode = cache.current_mode.clone();
                        cache.by_mode.get_mut(&current_mode)
                    });
                    if let Some(state) = current_state {
                        // Merge into the cached state so the panel keeps the
                        // last known value for whichever part is absent.
                        if let Some(shutter_speed) = shutter_speed {
                            state.effective_shutter_speed = shutter_speed;
                        }
                        if let Some(aperture) = aperture {
                            state.effective_aperture = aperture;
                        }
                        update_current_shutter_speed_display(
                            &current_shutter_speed_display,
                            &state.display_exposure(),
                        );
                    } else {
                        let state = CaptureSettingsState {
                            effective_shutter_speed: shutter_speed.unwrap_or_default(),
                            effective_aperture: aperture.unwrap_or_default(),
                            ..CaptureSettingsState::default()
                        };
                        update_current_shutter_speed_display(
                            &current_shutter_speed_display,
                            &state.display_exposure(),
                        );
                    }
                }
                LiveViewEvent::Error(error) => {
                    if !startup_complete {
                        stop_starting_animation(&startup_logo, &startup_blink_source);
                        update_connection_state(
                            ContentViewState::Disconnected,
                            &status_label,
                            &connect_action,
                            &disconnect_action,
                            &capture_action,
                            &focus_action,
                            &content_stack,
                        );
                    }
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
            pending_added_contents_worker,
        )) && !stop_worker.load(Ordering::Relaxed)
        {
            let _ = sender.send(LiveViewEvent::Error(error));
        }
    });

    LiveViewSession {
        stop,
        child_pid,
        session_cookie,
        pending_added_contents,
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
    pending_added_contents: Arc<Mutex<Vec<String>>>,
) -> Result<(), String> {
    let base_url = format!(
        "http://{}:{}",
        configured_camera.host, configured_camera.port
    );
    log_live_view(format!("live-view session base URL: {base_url}"));
    let session_cookie = establish_browser_remote_session(&base_url, &configured_camera)?;
    if let Ok(mut slot) = session_cookie_slot.lock() {
        *slot = Some(session_cookie.clone());
    }
    log_live_view(format!(
        "browser remote login succeeded; cookie prefix={}",
        session_cookie.split('=').next().unwrap_or_default()
    ));

    let initial_capture_settings =
        prepare_browser_remote_shooting_page(&base_url, &session_cookie, &sender)?;
    if let Some(initial_capture_settings) = initial_capture_settings {
        let (cache, restored_state) = build_capture_settings_cache(initial_capture_settings);
        let _ = sender.send(LiveViewEvent::CaptureSettingsCache(cache));
        let _ = sender.send(LiveViewEvent::CaptureSettings(restored_state));
    }
    let polling_stop = Arc::new(AtomicBool::new(false));
    start_event_polling(
        base_url.clone(),
        session_cookie.clone(),
        sender.clone(),
        stop.clone(),
        polling_stop.clone(),
        pending_added_contents,
    );

    // The Browser Remote transport starts when `/brapi/shooting/
    // lvscrolldetail?...` is opened; resetting stale transport state before
    // each reconnect is more reliable than proactively toggling the CCAPI
    // live-view state here.
    let mut consecutive_failures = 0;
    let mut busy_retries = 0;
    let stream_result = loop {
        if let Err(error) = stop_live_view_transport(&base_url, &session_cookie) {
            log_live_view(format!(
                "live view transport stop failed before open: {error}"
            ));
        }
        let result = stream_live_view(
            &base_url,
            &session_cookie,
            sender.clone(),
            stop.clone(),
            child_pid.clone(),
        );
        if stop.load(Ordering::Relaxed) {
            break result;
        }
        match result {
            Ok(()) => {
                consecutive_failures = 0;
                busy_retries = 0;
                log_live_view("live-view stream ended; reconnecting");
            }
            Err(error) => {
                if error_indicates_camera_busy(&error) {
                    busy_retries += 1;
                    if busy_retries >= 60 {
                        break Err(error);
                    }
                    log_live_view(format!(
                        "camera busy for live view (retry #{busy_retries}): {error}"
                    ));
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }

                if error_indicates_live_view_already_started(&error) {
                    consecutive_failures += 1;
                    if consecutive_failures >= 6 {
                        break Err(error);
                    }
                    log_live_view(format!(
                        "live view transport already started (retry #{consecutive_failures})"
                    ));
                    if let Err(stop_error) = stop_live_view_transport(&base_url, &session_cookie) {
                        log_live_view(format!(
                            "live view transport stop after already-started failed: {stop_error}"
                        ));
                    }
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                consecutive_failures += 1;
                if consecutive_failures >= 3 {
                    break Err(error);
                }
                log_live_view(format!(
                    "live-view stream error (retry #{consecutive_failures}): {error}"
                ));
            }
        }
        thread::sleep(Duration::from_millis(500));
    };
    polling_stop.store(true, Ordering::Relaxed);
    stream_result?;

    if !stop.load(Ordering::Relaxed) {
        let _ = stop_live_view_transport(&base_url, &session_cookie);
        let _ = logout_browser_remote(&base_url, &configured_camera, Some(&session_cookie));
    }

    if let Ok(mut slot) = session_cookie_slot.lock() {
        *slot = None;
    }

    Ok(())
}

fn establish_browser_remote_session(
    base_url: &str,
    configured_camera: &ConfiguredCamera,
) -> Result<String, String> {
    const MAX_LOGIN_ATTEMPTS: usize = 3;
    const LOGIN_RETRY_DELAY: Duration = Duration::from_millis(500);

    match login_browser_remote(base_url, configured_camera) {
        Ok(session_cookie) => return Ok(session_cookie),
        Err(BrowserRemoteLoginError::AlreadyInUse) => {
            log_live_view("browser remote already in use; forcing session reset");
        }
        Err(error) => return Err(error.into_message()),
    }

    for attempt in 1..=MAX_LOGIN_ATTEMPTS {
        if let Err(error) = logout_browser_remote(base_url, configured_camera, None) {
            log_live_view(format!(
                "browser remote logout reset attempt #{attempt} failed: {error}"
            ));
        }

        match login_browser_remote(base_url, configured_camera) {
            Ok(session_cookie) => return Ok(session_cookie),
            Err(BrowserRemoteLoginError::AlreadyInUse) if attempt < MAX_LOGIN_ATTEMPTS => {
                log_live_view(format!(
                    "browser remote still in use after reset; retrying (attempt #{attempt})"
                ));
                thread::sleep(LOGIN_RETRY_DELAY);
            }
            Err(error) => return Err(error.into_message()),
        }
    }

    Err("Browser Remote is already in use".to_owned())
}

fn logout_browser_remote(
    base_url: &str,
    configured_camera: &ConfiguredCamera,
    session_cookie: Option<&str>,
) -> Result<(), String> {
    let mut command = Command::new("curl");
    command.args([
        "-sS",
        "-L",
        "-b",
        "",
        "-c",
        "/dev/null",
        "-D",
        "-",
        "-o",
        "/dev/null",
    ]);

    if let Some((username, password)) = configured_camera.credentials() {
        command.args(["--digest", "-u", &format!("{username}:{password}")]);
    }

    if let Some(cookie) = session_cookie {
        command.args(["-H", &format!("Cookie: {cookie}")]);
    }

    command.arg(format!("{base_url}/brapi/logout"));
    let output = command
        .output()
        .map_err(|error| format!("failed to run curl for Browser Remote logout: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl Browser Remote logout failed: {stderr}"));
    }

    let headers = String::from_utf8_lossy(&output.stdout);
    log_live_view(format!(
        "browser remote logout headers:\n{}",
        headers.trim_end()
    ));
    Ok(())
}

fn login_browser_remote(
    base_url: &str,
    configured_camera: &ConfiguredCamera,
) -> Result<String, BrowserRemoteLoginError> {
    let (username, password) = configured_camera
        .credentials()
        .ok_or(BrowserRemoteLoginError::MissingCredentials)?;
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
        .map_err(|error| {
            BrowserRemoteLoginError::Curl(format!(
                "failed to run curl for Browser Remote login: {error}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BrowserRemoteLoginError::Curl(format!(
            "curl Browser Remote login failed: {stderr}"
        )));
    }

    let headers = String::from_utf8_lossy(&output.stdout);
    log_live_view(format!(
        "browser remote login headers:\n{}",
        headers.trim_end()
    ));
    parse_browser_remote_login_headers(&headers)
}

fn parse_browser_remote_login_headers(headers: &str) -> Result<String, BrowserRemoteLoginError> {
    let location = headers
        .lines()
        .filter_map(|line| line.strip_prefix("Location:"))
        .map(str::trim)
        .next_back()
        .unwrap_or_default();

    if location == "/wpd/already_login.shtml" {
        return Err(BrowserRemoteLoginError::AlreadyInUse);
    }

    if location != "/wpd/topmenu.shtml" {
        return Err(BrowserRemoteLoginError::UnexpectedLandingPage(
            location.to_owned(),
        ));
    }

    headers
        .lines()
        .filter_map(|line| line.strip_prefix("Set-Cookie:"))
        .map(str::trim)
        .filter_map(|line| line.split(';').next())
        .find(|cookie| cookie.starts_with("brsessionid="))
        .map(str::to_owned)
        .ok_or(BrowserRemoteLoginError::MissingSessionCookie)
}

fn prepare_browser_remote_shooting_page(
    base_url: &str,
    session_cookie: &str,
    sender: &mpsc::Sender<LiveViewEvent>,
) -> Result<Option<CaptureSettingsState>, String> {
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
        log_live_view_state_summary("currentproperty", &body);
        let capture_settings = parse_capture_settings_state(&body);
        if let Some(state) = parse_focus_mode_state(&body) {
            let _ = sender.send(LiveViewEvent::FocusMode(state));
        }
        if let Some(state) = capture_settings.clone() {
            let _ = sender.send(LiveViewEvent::CaptureSettings(state));
        }
        Ok(capture_settings)
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

fn summarize_live_view_state(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let mut parts = Vec::new();

    if let Some(message) = value.get("message").and_then(Value::as_str) {
        parts.push(format!("message={message}"));
    }

    if let Some(liveview) = value.get("liveview") {
        let liveviewsize = liveview
            .get("liveviewsize")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let cameradisplay = liveview
            .get("cameradisplay")
            .and_then(Value::as_str)
            .unwrap_or("-");
        parts.push(format!(
            "liveviewsize={liveviewsize} cameradisplay={cameradisplay}"
        ));
    }

    if let Some(imagereview) = value
        .get("imagereview")
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
    {
        parts.push(format!("imagereview={imagereview}"));
    }

    if let Some(mode) = value
        .get("shootingmode")
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
    {
        parts.push(format!("shootingmode={mode}"));
    }

    if let Some(moviemode) = value
        .get("moviemode")
        .and_then(|node| node.get("status"))
        .and_then(Value::as_str)
    {
        parts.push(format!("moviemode={moviemode}"));
    }

    if let Some(recbutton) = value
        .get("recbutton")
        .and_then(|node| node.get("status"))
        .and_then(Value::as_str)
    {
        parts.push(format!("recbutton={recbutton}"));
    }

    if let Some(recordableshots) = value
        .get("recordable")
        .and_then(|node| node.get("recordableshots"))
        .and_then(Value::as_i64)
    {
        parts.push(format!("recordableshots={recordableshots}"));
    }

    if let Some(remainingtime) = value
        .get("recordable")
        .and_then(|node| node.get("remainingtime"))
    {
        let remainingtime = match remainingtime {
            Value::Null => "null".to_owned(),
            Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        parts.push(format!("remainingtime={remainingtime}"));
    }

    if let Some(tv) = value
        .get("effective_value_tv")
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("effective_tv={tv}"));
    }

    if let Some(av) = value
        .get("effective_value_av")
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("effective_av={av}"));
    }

    (!parts.is_empty()).then(|| parts.join(" "))
}

fn log_live_view_state_summary(label: &str, body: &str) {
    if let Some(summary) = summarize_live_view_state(body) {
        log_live_view(format!("{label} state: {summary}"));
    }
}

fn parse_capture_settings_state(body: &str) -> Option<CaptureSettingsState> {
    let value: Value = serde_json::from_str(body).ok()?;
    let state = CaptureSettingsState {
        mode: parse_selectable_setting_state(&value, "shootingmode"),
        iso: parse_selectable_setting_state(&value, "iso"),
        shutter_speed: parse_selectable_setting_state(&value, "tv"),
        aperture: parse_selectable_setting_state(&value, "av"),
        effective_shutter_speed: parse_setting_value(&value, "effective_value_tv"),
        effective_aperture: parse_setting_value(&value, "effective_value_av"),
    };

    (state.mode.is_available()
        || state.iso.is_available()
        || state.shutter_speed.is_available()
        || state.aperture.is_available())
    .then_some(state)
}

fn parse_setting_value(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn parse_selectable_setting_state(value: &Value, key: &str) -> SelectableSettingState {
    let current = value
        .get(key)
        .and_then(|node| node.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let ability = value
        .get(key)
        .and_then(|node| node.get("ability"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    SelectableSettingState { current, ability }
}

fn update_selectable_camera_setting(
    camera: &ConfiguredCamera,
    session_cookie: &str,
    setting_name: &str,
    value: &str,
) -> Result<(), String> {
    let base_url = format!("http://{}:{}", camera.host, camera.port);
    let referer = format!("{base_url}/wpd/shoot.shtml");
    let body = format!(r#"{{"value":"{value}"}}"#);
    let (status, response_body) = run_curl_request(
        "PUT",
        &format!("{base_url}/ccapi/ver100/shooting/settings/{setting_name}"),
        Some(session_cookie),
        Some(&referer),
        Some(&body),
    )?;
    log_live_view(format!(
        "setting update status={status} setting={setting_name} value={value} body_prefix={}",
        preview_text(&response_body)
    ));

    if status == 200 {
        Ok(())
    } else {
        Err(format!(
            "setting update failed for `{setting_name}` with {status}: {response_body}"
        ))
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
    log_live_view(format!(
        "opening live-view stream {}{}",
        base_url, LIVE_VIEW_STREAM
    ));
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--no-buffer",
            "--dump-header",
            "/dev/stderr",
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
    // The camera sometimes stops rendering live view (e.g. after a shooting
    // mode change) while keeping the stream open and delivering tiny blank
    // frames; watch for that and re-enable rendering.
    const BLANK_STREAK_THRESHOLD: u64 = 30;
    const REENABLE_RETRY_FRAMES: u64 = 90;
    let mut blank_frame_streak = 0_u64;
    let mut next_reenable_at = BLANK_STREAK_THRESHOLD;

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
                if let Some(stderr) = stderr.as_mut() {
                    let mut err = String::new();
                    let _ = stderr.read_to_string(&mut err);
                    if !err.trim().is_empty() {
                        log_live_view(format!(
                            "live-view stream transport stderr_prefix={}",
                            preview_text(&err)
                        ));
                    }
                }
                log_live_view_state_summary("live-view stream JSON response", &body);
                log_live_view_currentproperty_debug_snapshot(
                    base_url,
                    session_cookie,
                    "stream-json-response",
                );
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
        drain_live_view_frames(
            &mut buffer,
            &sender,
            &mut parsed_frames,
            &mut blank_frame_streak,
        );

        if blank_frame_streak == 0 {
            next_reenable_at = BLANK_STREAK_THRESHOLD;
        } else if blank_frame_streak >= next_reenable_at {
            log_live_view(format!(
                "re-enabling live view after {blank_frame_streak} blank frames"
            ));
            match enable_live_view(base_url, session_cookie) {
                Ok(LiveViewEnableOutcome::Enabled | LiveViewEnableOutcome::Busy) => {}
                Err(error) => log_live_view(format!("live view enable failed: {error}")),
            }
            next_reenable_at = blank_frame_streak + REENABLE_RETRY_FRAMES;
        }
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

fn start_event_polling(
    base_url: String,
    session_cookie: String,
    sender: mpsc::Sender<LiveViewEvent>,
    stop: Arc<AtomicBool>,
    polling_stop: Arc<AtomicBool>,
    pending_added_contents: Arc<Mutex<Vec<String>>>,
) {
    // Browser Remote's shoot page keeps a `/ccapi/ver100/event/polling` loop
    // running next to the live-view stream; it is the channel that delivers
    // ongoing value changes such as the metered shutter speed in P/Av modes.
    thread::spawn(move || {
        const POLL_PAUSE: Duration = Duration::from_millis(500);

        let referer = format!("{base_url}/wpd/shoot.shtml");
        let mut polls = 0_u64;
        log_live_view("event polling started");
        while !stop.load(Ordering::Relaxed) && !polling_stop.load(Ordering::Relaxed) {
            match run_event_polling_request(&base_url, &session_cookie, &referer) {
                Ok((200, body)) => {
                    polls += 1;
                    let added_contents = parse_added_contents(&body);
                    if !added_contents.is_empty() {
                        log_live_view(format!(
                            "event poll #{polls} added contents: {}",
                            added_contents.join(", ")
                        ));
                        if let Ok(mut pending) = pending_added_contents.lock() {
                            pending.extend(added_contents);
                        }
                    }
                    if polls <= 5 {
                        log_live_view(format!(
                            "event poll #{polls} body_prefix={}",
                            preview_text(&body)
                        ));
                        log_live_view_state_summary(&format!("event poll #{polls}"), &body);
                    }
                    // The camera reports its own live-view rendering state
                    // here; log it to trace live view going blank.
                    if let Ok(value) = serde_json::from_str::<Value>(&body)
                        && let Some(liveview) = value.get("liveview")
                    {
                        log_live_view(format!("event poll #{polls} liveview state={liveview}"));
                    }
                    let (shutter_speed, aperture) = parse_event_effective_exposure(&body);
                    if shutter_speed.is_some() || aperture.is_some() {
                        log_live_view(format!(
                            "event poll #{polls} effective exposure tv={} av={}",
                            shutter_speed.as_deref().unwrap_or("-"),
                            aperture.as_deref().unwrap_or("-")
                        ));
                        let _ = sender.send(LiveViewEvent::EffectiveExposure {
                            shutter_speed,
                            aperture,
                        });
                    }
                }
                Ok((status, body)) => {
                    log_live_view(format!(
                        "event polling stopped after status={status} body_prefix={}",
                        preview_text(&body)
                    ));
                    break;
                }
                Err(_) => {
                    // The long poll timing out without events is the idle case.
                }
            }
            thread::sleep(POLL_PAUSE);
        }
        log_live_view("event polling stopped");
    });
}

fn run_event_polling_request(
    base_url: &str,
    session_cookie: &str,
    referer: &str,
) -> Result<(u16, String), String> {
    // Bounded so the polling thread can notice the stop flags even when the
    // camera holds the long poll open.
    let output = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "5",
            "-H",
            &format!("Cookie: {session_cookie}"),
            "-e",
            referer,
            "-w",
            "\n__STATUS__:%{http_code}",
            &format!("{base_url}/ccapi/ver100/event/polling?continue=on"),
        ])
        .output()
        .map_err(|error| format!("failed to run curl for event polling: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("event polling request failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (body, status) = stdout
        .rsplit_once("\n__STATUS__:")
        .ok_or_else(|| "event polling response missing status marker".to_owned())?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|error| format!("invalid event polling status: {error}"))?;
    Ok((status, body.to_owned()))
}

fn parse_event_effective_exposure(body: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    // The `effective_value_*` keys carry the metered values in modes where
    // the camera estimates them; `tv`/`av` carry user-selected ones.
    let extract = |keys: [&str; 2]| {
        keys.iter().find_map(|key| {
            let setting_value = value.get(key)?.get("value")?.as_str()?.trim();
            (!setting_value.is_empty()).then(|| setting_value.to_owned())
        })
    };
    (
        extract(["effective_value_tv", "tv"]),
        extract(["effective_value_av", "av"]),
    )
}

fn log_live_view_currentproperty_debug_snapshot(
    base_url: &str,
    session_cookie: &str,
    context: &str,
) {
    let referer = format!("{base_url}/wpd/shoot.shtml");
    match run_curl_request(
        "GET",
        &format!("{base_url}/brapi/currentproperty"),
        Some(session_cookie),
        Some(&referer),
        None,
    ) {
        Ok((status, body)) => {
            log_live_view(format!(
                "{context} currentproperty status={status} body_prefix={}",
                preview_text(&body)
            ));
            log_live_view_state_summary(&format!("{context} currentproperty"), &body);
        }
        Err(error) => {
            log_live_view(format!(
                "{context} currentproperty debug snapshot failed: {error}"
            ));
        }
    }
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
    pending_added_contents: &Arc<Mutex<Vec<String>>>,
) -> Result<CaptureOutcome, String> {
    let contents = wait_for_added_contents(pending_added_contents, media_kind)?;

    if storage == StorageMode::CameraOnly {
        return Ok(CaptureOutcome {
            status_message: match media_kind {
                CapturedMediaKind::Picture => "Picture captured on camera.".to_owned(),
                CapturedMediaKind::Video => "Video captured on camera.".to_owned(),
            },
            path_labels: contents,
        });
    }

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

    Ok(CaptureOutcome {
        status_message: match storage {
            StorageMode::CameraOnly => unreachable!("camera only is returned early"),
            StorageMode::WorkspaceOnly => {
                format!("{noun} downloaded to {destination} and removed from camera.")
            }
            StorageMode::Both => {
                format!("{noun} captured on camera and downloaded to {destination}.")
            }
        },
        path_labels: downloaded_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

fn wait_for_added_contents(
    pending_added_contents: &Arc<Mutex<Vec<String>>>,
    media_kind: CapturedMediaKind,
) -> Result<Vec<String>, String> {
    const MAX_ATTEMPTS: usize = 20;
    const POLL_DELAY: Duration = Duration::from_millis(500);

    for attempt in 1..=MAX_ATTEMPTS {
        let matching_contents = take_matching_added_contents(pending_added_contents, media_kind);
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

fn clear_pending_added_contents(pending_added_contents: &Arc<Mutex<Vec<String>>>) {
    if let Ok(mut pending) = pending_added_contents.lock() {
        pending.clear();
    }
}

fn take_matching_added_contents(
    pending_added_contents: &Arc<Mutex<Vec<String>>>,
    media_kind: CapturedMediaKind,
) -> Vec<String> {
    let Ok(mut pending) = pending_added_contents.lock() else {
        return Vec::new();
    };

    let mut matching = Vec::new();
    let mut remaining = Vec::new();
    for content in pending.drain(..) {
        if filter_added_contents_by_media_kind(vec![content.clone()], media_kind).is_empty() {
            remaining.push(content);
        } else {
            matching.push(content);
        }
    }
    *pending = remaining;
    matching
}

fn parse_added_contents(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("addedcontents").cloned())
        .and_then(|value| value.as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn filter_added_contents_by_media_kind(
    contents: Vec<String>,
    media_kind: CapturedMediaKind,
) -> Vec<String> {
    contents
        .into_iter()
        .filter(|content| match media_kind {
            CapturedMediaKind::Picture => {
                has_content_extension(content, PICTURE_CONTENT_EXTENSIONS)
            }
            CapturedMediaKind::Video => has_content_extension(content, VIDEO_CONTENT_EXTENSIONS),
        })
        .collect()
}

fn has_content_extension(content_path: &str, extensions: &[&str]) -> bool {
    let lower = content_path.to_ascii_lowercase();
    extensions
        .iter()
        .any(|extension| lower.ends_with(&format!(".{extension}")))
}

fn workspace_media_matches_kind(path: &Path, media_kind: CapturedMediaKind) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    let extensions = match media_kind {
        CapturedMediaKind::Picture => PICTURE_CONTENT_EXTENSIONS,
        CapturedMediaKind::Video => VIDEO_CONTENT_EXTENSIONS,
    };
    extensions.iter().any(|candidate| extension == *candidate)
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
    blank_frame_streak: &mut u64,
) {
    // The camera renders "no live view" as a tiny (<1 KiB) black JPEG; real
    // frames are two orders of magnitude larger.
    const BLANK_FRAME_MAX_BYTES: usize = 10_240;
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
            if payload.len() < BLANK_FRAME_MAX_BYTES {
                *blank_frame_streak += 1;
                if *blank_frame_streak == 1 {
                    log_live_view(format!(
                        "live view went blank (frame #{} {} bytes)",
                        *parsed_frames,
                        payload.len()
                    ));
                }
            } else {
                if *blank_frame_streak > 0 {
                    log_live_view(format!(
                        "live view recovered after {} blank frames",
                        *blank_frame_streak
                    ));
                }
                *blank_frame_streak = 0;
            }
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

    clamp_focus_overlay_frame(&mut next)?;
    Some(next)
}

fn focus_target_x(state: &FocusOverlayState) -> i32 {
    (state.frame_x + state.frame_width / 2.0).round() as i32
}

fn focus_target_y(state: &FocusOverlayState) -> i32 {
    (state.frame_y + state.frame_height / 2.0).round() as i32
}

fn focus_overlay_state_from_click(
    state: &FocusOverlayState,
    available_width: f64,
    available_height: f64,
    click_x: f64,
    click_y: f64,
) -> Result<FocusOverlayState, &'static str> {
    if !focus_overlay_is_selectable(state, available_width, available_height) {
        return Err("Focus point unavailable.");
    }

    let (offset_x, offset_y, scaled_width, scaled_height) = contained_image_rect(
        available_width,
        available_height,
        state.image_width,
        state.image_height,
    );
    let image_right = offset_x + scaled_width;
    let image_bottom = offset_y + scaled_height;
    if click_x < offset_x || click_x > image_right || click_y < offset_y || click_y > image_bottom {
        return Err("Click inside the live view image to focus.");
    }

    let scale_x = scaled_width / state.image_width;
    let scale_y = scaled_height / state.image_height;
    let target_x = state.image_x + (click_x - offset_x) / scale_x;
    let target_y = state.image_y + (click_y - offset_y) / scale_y;

    let mut next = state.clone();
    next.frame_x = target_x - next.frame_width / 2.0;
    next.frame_y = target_y - next.frame_height / 2.0;
    clamp_focus_overlay_frame(&mut next).ok_or("Focus point unavailable.")?;
    Ok(next)
}

fn focus_overlay_is_selectable(
    state: &FocusOverlayState,
    available_width: f64,
    available_height: f64,
) -> bool {
    state.active
        && state.image_width > 0.0
        && state.image_height > 0.0
        && state.frame_width > 0.0
        && state.frame_height > 0.0
        && state.frame_width <= state.image_width
        && state.frame_height <= state.image_height
        && available_width > 0.0
        && available_height > 0.0
}

fn clamp_focus_overlay_frame(state: &mut FocusOverlayState) -> Option<()> {
    focus_overlay_is_selectable(state, 1.0, 1.0).then_some(())?;

    state.frame_x = state.frame_x.clamp(
        state.image_x,
        state.image_x + state.image_width - state.frame_width,
    );
    state.frame_y = state.frame_y.clamp(
        state.image_y,
        state.image_y + state.image_height - state.frame_height,
    );
    Some(())
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

fn error_indicates_camera_busy(error: &str) -> bool {
    error.contains("During shooting or recording")
}

fn error_indicates_live_view_already_started(error: &str) -> bool {
    error.contains("Already started")
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

    #[test]
    fn derives_camera_scan_mask_from_ipv4_host() {
        assert_eq!(
            default_camera_scan_mask("192.168.1.23"),
            "192.168.1.xxx".to_owned()
        );
    }

    #[test]
    fn leaves_camera_scan_mask_empty_for_non_ipv4_hosts() {
        assert!(default_camera_scan_mask("camera.local").is_empty());
    }

    #[test]
    fn maps_live_view_click_to_focus_target() {
        let state = FocusOverlayState {
            image_x: 0.0,
            image_y: 0.0,
            image_width: 200.0,
            image_height: 100.0,
            frame_x: 40.0,
            frame_y: 30.0,
            frame_width: 20.0,
            frame_height: 10.0,
            active: true,
        };

        let next = focus_overlay_state_from_click(&state, 400.0, 200.0, 300.0, 100.0).unwrap();

        assert_eq!(focus_target_x(&next), 150);
        assert_eq!(focus_target_y(&next), 50);
    }

    #[test]
    fn rejects_live_view_clicks_outside_the_image() {
        let state = FocusOverlayState {
            image_x: 0.0,
            image_y: 0.0,
            image_width: 200.0,
            image_height: 100.0,
            frame_x: 40.0,
            frame_y: 30.0,
            frame_width: 20.0,
            frame_height: 10.0,
            active: true,
        };

        let error = focus_overlay_state_from_click(&state, 400.0, 300.0, 10.0, 10.0).unwrap_err();

        assert_eq!(error, "Click inside the live view image to focus.");
    }

    #[test]
    fn parses_browser_remote_login_cookie_from_headers() {
        let cookie = parse_browser_remote_login_headers(
            "HTTP/1.1 401 Unauthorized\n\
             WWW-Authenticate: Digest realm=\"BrowserRemote\"\n\
             \n\
             HTTP/1.1 303 See Other\n\
             Location:/wpd/topmenu.shtml\n\
             Set-Cookie: brsessionid=abc123; Path=/; HttpOnly\n",
        )
        .unwrap();

        assert_eq!(cookie, "brsessionid=abc123");
    }

    #[test]
    fn reports_browser_remote_login_collision() {
        let error = parse_browser_remote_login_headers(
            "HTTP/1.1 401 Unauthorized\n\
             WWW-Authenticate: Digest realm=\"BrowserRemote\"\n\
             \n\
             HTTP/1.1 303 See Other\n\
             Location:/wpd/already_login.shtml\n",
        )
        .unwrap_err();

        assert_eq!(error, BrowserRemoteLoginError::AlreadyInUse);
    }

    #[test]
    fn detects_camera_busy_errors() {
        assert!(error_indicates_camera_busy(
            "live view stream returned body: {\"message\":\"During shooting or recording\"}"
        ));
        assert!(!error_indicates_camera_busy(
            "unexpected Browser Remote landing page"
        ));
    }

    #[test]
    fn detects_live_view_already_started_errors() {
        assert!(error_indicates_live_view_already_started(
            "live view stream returned body: {\"message\":\"Already started\"}"
        ));
        assert!(!error_indicates_live_view_already_started(
            "live view stream ended without decoded frames"
        ));
    }

    #[test]
    fn seeds_capture_settings_cache_with_current_mode_only() {
        let state = CaptureSettingsState {
            mode: SelectableSettingState {
                current: "av".to_owned(),
                ability: vec!["fv".to_owned(), "av".to_owned(), "m".to_owned()],
            },
            iso: SelectableSettingState {
                current: "800".to_owned(),
                ability: vec!["auto".to_owned(), "800".to_owned()],
            },
            ..CaptureSettingsState::default()
        };

        let (cache, restored_state) = build_capture_settings_cache(state.clone());

        assert_eq!(restored_state.mode.current, "av");
        assert_eq!(cache.current_mode, "av");
        assert_eq!(cache.by_mode.len(), 1);
        assert!(cache.by_mode.contains_key("av"));
    }

    #[test]
    fn summarizes_live_view_state_fields() {
        let summary = summarize_live_view_state(
            r#"{
                "message":"Already started",
                "liveview":{"liveviewsize":"medium","cameradisplay":"on"},
                "imagereview":{"value":"2"},
                "shootingmode":{"value":"av"},
                "moviemode":{"status":"off"},
                "recbutton":{"status":"stop"},
                "recordable":{"recordableshots":5834,"remainingtime":null},
                "effective_value_av":{"value":"f4.5"}
            }"#,
        )
        .unwrap();

        assert!(summary.contains("message=Already started"));
        assert!(summary.contains("liveviewsize=medium cameradisplay=on"));
        assert!(summary.contains("imagereview=2"));
        assert!(summary.contains("recordableshots=5834"));
        assert!(summary.contains("remainingtime=null"));
    }
}
