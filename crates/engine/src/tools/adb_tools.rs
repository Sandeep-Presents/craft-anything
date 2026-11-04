//! A connected Android phone or emulator, through the user's `adb`.
//!
//! Every tool runs `adb` via [`craft_anything_platform::process`] (closed stdin, wall and idle
//! deadlines, capped output, process-group kill on timeout/cancel/drop) and returns an [`Evidence`]
//! record on the authority of an [`Authority::OutsideService`] (the device). Serials, package
//! names, settings keys, intent actions and components are validated against strict allowlists;
//! device paths and extra values are single-quoted before they reach the device shell. Device-paths
//! never become part of a host shell string — adb arguments are passed as argv.
//!
//! Tools that write to the device (`push`, `install`, `uninstall`, `start`, `stop`) carry
//! [`Effects::MUTATES_DEVICE`] and are consent-gated; tools that write to the host carry
//! [`Effects::WRITES_FILESYSTEM`]. When adb isn't installed every tool (except `engines_adb_check`,
//! which reports exactly that) returns a clear `EngineMissing`; when the device is missing or
//! unauthorised they return `NotFound` / `NoPermission`. Nothing ever hangs.

use super::artifact_tools::{UnlessWritten, create_absent_dir};
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::adb::{self, Adb, Device};
use craft_anything_engines::adb_ui;
use craft_anything_evidence::{Authority, Confidence, Coverage, Environment, ErrorCode, Evidence, EvidenceBuilder, Limitation, Location, Subject, ToolError};
use craft_anything_platform::fs::write_atomic;
use craft_anything_platform::process::{Cancel, Output};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub fn specs() -> Vec<ToolSpec> {
    use Toolset::Adb;
    // Base effects for a read-only device query: records evidence, runs adb, talks to the device
    // over adb's socket.
    let read = Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK;
    let host_write = read | Effects::WRITES_FILESYSTEM;
    let device_write = read | Effects::MUTATES_DEVICE; // MUTATES_DEVICE → consent-gated
    let eng = Requires::Engine("adb");
    vec![
        spec(
            "engines_adb_check",
            "Is adb available?",
            "Report whether Android platform-tools (adb) is installed (Settings path, the managed engine store, then PATH) and its version. Contacts no device. Needs nothing installed to answer.",
            "Check whether the helper that talks to Android phones is installed.",
            Adb,
            Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS,
            Requires::Nothing,
            schema_of::<Empty>,
            engines_adb_check,
        ),
        spec(
            "adb_devices_find",
            "Connected devices",
            "List phones and emulators attached to adb (`adb devices -l`): serial, connection state (device, unauthorized, offline…), product, model and transport id.",
            "See which Android phones or emulators are plugged in.",
            Adb,
            read,
            eng,
            schema_of::<Empty>,
            adb_devices_find,
        ),
        spec(
            "adb_device_show",
            "Device details",
            "Read a whitelist of device properties with `getprop` (manufacturer, brand, model, Android version and SDK level, security patch, CPU ABIs, build fingerprint/type). The full property set is parsed and counted; a bounded sample is attached.",
            "Find out what phone this is and which Android version it runs.",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_device_show,
        ),
        spec(
            "adb_apps_list",
            "Installed apps",
            "List installed package names with `pm list packages`. Scope all (default), third_party (-3) or system (-s).",
            "List the apps installed on the phone.",
            Adb,
            read,
            eng,
            schema_of::<ListPackagesParams>,
            adb_apps_list,
        ),
        spec(
            "adb_log_read",
            "Device log",
            "Read a bounded tail of the device log with `logcat -d` (dump and exit, never follow). count 1..10000 (default 500), buffer main|system|radio|events|crash (default main), optional pid filter. Lines are parsed into timestamp, pid, tid, level, tag and message.",
            "Read recent messages the phone printed, so you can see what an app is doing.",
            Adb,
            read,
            eng,
            schema_of::<LogcatParams>,
            adb_log_read,
        ),
        spec(
            "adb_app_show",
            "App details on device",
            "Inspect one installed package with `dumpsys package <name>`: version name/code, userId, install and update times, data and code paths, declared and granted permissions. Key fields are parsed; a bounded raw tail is attached for the rest.",
            "Look at one installed app's version, permissions and where it lives on the phone.",
            Adb,
            read,
            eng,
            schema_of::<PackageParams>,
            adb_app_show,
        ),
        spec(
            "adb_running_processes_list",
            "Running processes",
            "List running processes with `ps -A`, parsed into user, pid, ppid and name using the column header.",
            "See what is running on the phone right now.",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_running_processes_list,
        ),
        spec(
            "adb_folder_list",
            "Device folder",
            "List a folder on the device with `ls -la` (the path is single-quoted for the device shell). Raw lines are kept; a best-effort parse of name, size and permissions is attached.",
            "Look at the files in a folder on the phone.",
            Adb,
            read,
            eng,
            schema_of::<DirectoryParams>,
            adb_folder_list,
        ),
        spec(
            "adb_device_features_list",
            "Device features",
            "List hardware and software features the device reports with `pm list features`.",
            "See what the phone's hardware can do (camera, NFC, sensors…).",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_device_features_list,
        ),
        spec(
            "adb_system_services_list",
            "System services",
            "List registered system services with `service list`: index, name and the AIDL interface each exposes.",
            "See the phone's built-in system services.",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_system_services_list,
        ),
        spec(
            "adb_display_show",
            "Display info",
            "Read display state with `dumpsys display`. Key lines (display ids, real size, density, state) are surfaced; a bounded raw tail is attached.",
            "Find out about the phone's screen size and density.",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_display_show,
        ),
        spec(
            "adb_window_show",
            "Window / focus info",
            "Read window-manager state with `dumpsys window`. Focused window and app, and current activity lines are surfaced; a bounded raw tail is attached.",
            "See which app and screen is in front on the phone.",
            Adb,
            read,
            eng,
            schema_of::<SerialParams>,
            adb_window_show,
        ),
        spec(
            "adb_setting_get",
            "Read a setting",
            "Read one settings value with `settings get <namespace> <key>`. namespace system|secure|global; key is validated.",
            "Read one of the phone's settings.",
            Adb,
            read,
            eng,
            schema_of::<SettingParams>,
            adb_setting_get,
        ),
        spec(
            "adb_apps_find",
            "Find an app",
            "Find installed packages whose name contains a search term with `pm list packages <query>`.",
            "Search the phone's apps by name.",
            Adb,
            read,
            eng,
            schema_of::<ResolveParams>,
            adb_apps_find,
        ),
        spec(
            "adb_app_download",
            "Copy an app off the phone",
            "Copy an installed app's APK file(s) to a host folder: resolve paths with `pm path`, then `adb pull` each. output_directory must be absolute and must not exist yet; it is created fresh.",
            "Copy an app's installer file from the phone to your computer.",
            Adb,
            host_write,
            eng,
            schema_of::<PullPackageParams>,
            adb_app_download,
        ),
        spec(
            "adb_file_download",
            "Copy a file off the phone",
            "Copy a file or folder from the device to a host folder with `adb pull`. output_directory must be absolute and fresh.",
            "Copy a file from the phone to your computer.",
            Adb,
            host_write,
            eng,
            schema_of::<PullFileParams>,
            adb_file_download,
        ),
        spec(
            "adb_screenshot_take",
            "Screenshot",
            "Take a screenshot with `exec-out screencap -p` and write screen.png to a fresh host folder. output_directory must be absolute and must not exist yet.",
            "Take a picture of what's on the phone's screen.",
            Adb,
            host_write,
            eng,
            schema_of::<OutputDirParams>,
            adb_screenshot_take,
        ),
        spec(
            "adb_bugreport_save",
            "Bug report",
            "Collect a full bug report with `adb bugreport` into a fresh host folder (this can take minutes). output_directory must be absolute and must not exist yet.",
            "Gather a big diagnostic report from the phone (takes a while).",
            Adb,
            host_write,
            eng,
            schema_of::<OutputDirParams>,
            adb_bugreport_save,
        ),
        spec(
            "adb_file_upload",
            "Copy a file to the phone",
            "Copy a host file to the device with `adb push`. local_path is an absolute host file; device_path is an absolute device path. With overwrite=false the tool refuses if the device path already exists. Changes the device.",
            "Copy a file from your computer onto the phone.",
            Adb,
            device_write,
            eng,
            schema_of::<PushParams>,
            adb_file_upload,
        ),
        spec(
            "adb_app_install",
            "Install an app",
            "Install an APK on the device with `adb install` (replace=true adds -r). apk_path is an absolute host file. Changes the device.",
            "Install an app onto the phone from an installer file.",
            Adb,
            device_write,
            eng,
            schema_of::<InstallParams>,
            adb_app_install,
        ),
        spec(
            "adb_app_remove",
            "Remove an app",
            "Uninstall a package with `adb uninstall`. Changes the device.",
            "Remove an app from the phone.",
            Adb,
            device_write,
            eng,
            schema_of::<PackageParams>,
            adb_app_remove,
        ),
        spec(
            "adb_app_launch",
            "Launch an app",
            "Launch a package's default activity with `monkey -p <pkg> -c android.intent.category.<CATEGORY> 1`: category `launcher` (the default, a phone's home screen entry) or `leanback` (a TV's). With the default, an app that has only a TV entry is launched by that instead. Changes the device (starts an app).",
            "Open an app on the phone or TV.",
            Adb,
            device_write,
            eng,
            schema_of::<LaunchParams>,
            adb_app_launch,
        ),
        spec(
            "adb_activity_open",
            "Start an activity",
            "Start an activity with `am start`: action (validated), optional data_uri (single-quoted), optional component (pkg/.Activity), and as many as 16 extras, each a text, true/false, whole or decimal number value. Changes the device.",
            "Open a specific screen in an app on the phone.",
            Adb,
            device_write,
            eng,
            schema_of::<ActivityParams>,
            adb_activity_open,
        ),
        spec(
            "adb_app_force_stop",
            "Force-stop an app",
            "Force-stop a package with `am force-stop`. Changes the device.",
            "Make an app on the phone stop running.",
            Adb,
            device_write,
            eng,
            schema_of::<PackageParams>,
            adb_app_force_stop,
        ),
        // The screen as a person sees and drives it (R2.12): keys, typed text, the controls on
        // screen, a state (keys, dump and picture in one go), and crashes. Nothing inside the app
        // is read, so these run in Clean room.
        spec(
            "adb_keys_press",
            "Press keys",
            "Send keys the way a remote, keyboard or game pad does, with `input keyevent`: an allowlisted key name per token (DPAD_RIGHT, DPAD_CENTER or OK, BACK, MENU, INFO, CHANNEL_UP, MEDIA_PLAY_PAUSE, digits, letters…), `x3` after a name to repeat it, gap_ms between presses (default 300) and wait_ms after the last. At most 200 presses per call. Changes the device (the app on screen reacts).",
            "Press buttons on the phone or TV as if with its remote.",
            Adb,
            device_write,
            eng,
            schema_of::<KeysParams>,
            adb_keys_press,
        ),
        spec(
            "adb_text_type",
            "Type text",
            "Type printable ASCII text into the focused field with `input text` (1 to 256 characters, no `%`). Changes the device.",
            "Type words into the field that has the cursor on the phone or TV.",
            Adb,
            device_write,
            eng,
            schema_of::<TextParams>,
            adb_text_type,
        ),
        spec(
            "adb_ui_read",
            "Read the screen's controls",
            "Read the controls on screen with `uiautomator dump`, in the shape runtime_ui_read gives (role, title, identifier, value, depth, position and size in pixels, and in dp when the display's density is known), with what a remote cares about: focused, focusable, selected, clickable, scrollable, enabled. Password fields' text is never read. With output_directory (absolute, must not exist yet) the tree is also written as ui.json.",
            "List what's on the phone or TV's screen: every control, which one has the focus, where each sits.",
            Adb,
            host_write,
            eng,
            schema_of::<UiReadParams>,
            adb_ui_read,
        ),
        spec(
            "adb_screen_state_capture",
            "Capture a screen state",
            "Press keys (optional, as adb_keys_press takes them), wait, then read the screen's controls and take a screenshot, all in one device-shell call, into a fresh host folder: ui.json, screen.png and keys.txt. dump_lead_ms starts the dump that many ms before the last key, so a short-lived overlay (a number entry, a channel banner) is read right after the key instead of 2-3 s later. The result names the focused control and a fingerprint of the screen's structure (bounds left out), so two captures of the same screen match. Changes the device when keys are given.",
            "Press some buttons, then record what the screen shows: its controls and a picture.",
            Adb,
            device_write | Effects::WRITES_FILESYSTEM,
            eng,
            schema_of::<StateCaptureParams>,
            adb_screen_state_capture,
        ),
        spec(
            "adb_crash_log_read",
            "Crashes of an app",
            "Read the device's crash log (`logcat -b crash`) for one package: the fatal exceptions and native crashes that name it, newest last, up to count lines read (default 500). `crashed` says whether any was found.",
            "See whether an app on the phone or TV crashed, and why.",
            Adb,
            read,
            eng,
            schema_of::<CrashLogParams>,
            adb_crash_log_read,
        ),
        spec(
            "adb_ui_walkthrough_record",
            "Walk through an app's screens",
            "Run a scripted walkthrough on the screen as it is: up to 40 steps, each some keys (as adb_keys_press takes them), a wait and a dump lead of its own, and after each a capture of the screen's controls and a picture (as adb_screen_state_capture makes them) into steps/NN-<name>/ of a fresh host folder, with walkthrough.json listing every step's focused control and fingerprint. Changes the device.",
            "Press a planned series of buttons on the phone or TV and record what the screen shows after each.",
            Adb,
            device_write | Effects::WRITES_FILESYSTEM,
            eng,
            schema_of::<WalkthroughParams>,
            adb_ui_walkthrough_record,
        ),
        spec(
            "adb_app_map",
            "Map an app's screens",
            "Map the screens an app reaches from its home-screen entry with a remote's keys, breadth first and bounded: from a fresh launch, each exploration key (default the D-pad, OK, BACK and MENU) is pressed on each known screen, the screen is read and pictured, and a screen not seen before (by the fingerprint of its controls, bounds left out) becomes a new state reached by that key path; every state is reached again from a fresh launch by its path, so the map is replayable. A screen whose text contains one of skip_texts is recorded as a row with no details and is never kept or explored further. Stops at max_states (default 20, at most 60), max_presses (default 400), time_budget_s (default 1800) or when nothing new is reached. Writes screens.json and states/<id>/{ui.json,screen.png,keys.txt} into a fresh host folder. Changes the device (the app is started and driven many times).",
            "Press every button on every screen of an app, within limits, and record the screens found.",
            Adb,
            device_write | Effects::WRITES_FILESYSTEM,
            eng,
            schema_of::<MapParams>,
            adb_app_map,
        ),
    ]
}

// ------------------------------------------------------------------------------------------------
// Params

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SerialParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PackageScope {
    #[default]
    All,
    ThirdParty,
    System,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListPackagesParams {
    serial: String,
    #[serde(default)]
    scope: PackageScope,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LogBuffer {
    #[default]
    Main,
    System,
    Radio,
    Events,
    Crash,
}

impl LogBuffer {
    fn name(self) -> &'static str {
        match self {
            LogBuffer::Main => "main",
            LogBuffer::System => "system",
            LogBuffer::Radio => "radio",
            LogBuffer::Events => "events",
            LogBuffer::Crash => "crash",
        }
    }
}

fn default_count() -> u32 {
    500
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogcatParams {
    serial: String,
    /// Number of recent lines (1..10000).
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default)]
    buffer: LogBuffer,
    /// Only lines from this process id.
    #[serde(default)]
    pid: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PackageParams {
    serial: String,
    /// Package name (e.g. com.example.app).
    package: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DirectoryParams {
    serial: String,
    /// Absolute device path (e.g. /sdcard/Download).
    device_path: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SettingNamespace {
    #[default]
    System,
    Secure,
    Global,
}

impl SettingNamespace {
    fn name(self) -> &'static str {
        match self {
            SettingNamespace::System => "system",
            SettingNamespace::Secure => "secure",
            SettingNamespace::Global => "global",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SettingParams {
    serial: String,
    #[serde(default)]
    namespace: SettingNamespace,
    /// Setting key (e.g. screen_brightness).
    key: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveParams {
    serial: String,
    /// Substring to match against package names.
    query: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OutputDirParams {
    serial: String,
    /// Absolute host folder that must not exist yet; it is created fresh.
    output_directory: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PullPackageParams {
    serial: String,
    package: String,
    output_directory: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PullFileParams {
    serial: String,
    device_path: String,
    output_directory: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PushParams {
    serial: String,
    /// Absolute host file to copy.
    local_path: String,
    /// Absolute destination path on the device.
    device_path: String,
    /// Refuse if the device path already exists (default false → adb push overwrites).
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstallParams {
    serial: String,
    /// Absolute host APK file.
    apk_path: String,
    /// Reinstall keeping data (adb install -r).
    #[serde(default = "default_true")]
    replace: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ExtraType {
    String,
    Boolean,
    Int,
    Long,
    Float,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Extra {
    key: String,
    #[serde(rename = "type")]
    ty: ExtraType,
    value: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActivityParams {
    serial: String,
    /// Intent action, e.g. android.intent.action.VIEW.
    #[serde(default)]
    action: Option<String>,
    /// Data URI (-d), single-quoted for the device shell.
    #[serde(default)]
    data_uri: Option<String>,
    /// Explicit component (-n), e.g. com.example.app/.MainActivity.
    #[serde(default)]
    component: Option<String>,
    /// Up to 16 typed intent extras.
    #[serde(default)]
    extras: Vec<Extra>,
}

/// Which home-screen entry `adb_app_launch` starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LaunchCategory {
    /// `android.intent.category.LAUNCHER`: a phone's or tablet's entry.
    #[default]
    Launcher,
    /// `android.intent.category.LEANBACK_LAUNCHER`: a TV's entry.
    Leanback,
}

impl LaunchCategory {
    fn intent(self) -> &'static str {
        match self {
            LaunchCategory::Launcher => "android.intent.category.LAUNCHER",
            LaunchCategory::Leanback => "android.intent.category.LEANBACK_LAUNCHER",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LaunchParams {
    serial: String,
    /// Package name (e.g. com.example.app).
    package: String,
    /// Which home-screen entry to start: `launcher` (default) or `leanback` (TV).
    #[serde(default)]
    category: LaunchCategory,
}

fn default_gap_ms() -> u32 {
    300
}

fn default_wait_ms() -> u32 {
    1000
}

fn default_max_nodes() -> usize {
    500
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeysParams {
    serial: String,
    /// Keys to press, in order: a key name per entry (`DPAD_RIGHT`, `OK`, `BACK`, `MENU`, `7`),
    /// with ` x3` after it to press it three times.
    keys: Vec<String>,
    /// Milliseconds between presses (0..10000, default 300).
    #[serde(default = "default_gap_ms")]
    gap_ms: u32,
    /// Milliseconds to wait after the last press before returning (0..60000, default 0).
    #[serde(default)]
    wait_ms: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TextParams {
    serial: String,
    /// Printable ASCII text, 1 to 256 characters, without `%`.
    text: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UiReadParams {
    serial: String,
    /// Most controls to return (1..5000, default 500).
    #[serde(default = "default_max_nodes")]
    max_nodes: usize,
    /// Absolute host folder that must not exist yet; when given, ui.json is written there.
    #[serde(default)]
    output_directory: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StateCaptureParams {
    serial: String,
    /// Keys to press first, as adb_keys_press takes them; none to capture the screen as it is.
    #[serde(default)]
    keys: Vec<String>,
    /// Milliseconds between presses (0..10000, default 300).
    #[serde(default = "default_gap_ms")]
    gap_ms: u32,
    /// Milliseconds to wait after the last press before reading the screen (0..60000, default 1000).
    #[serde(default = "default_wait_ms")]
    wait_ms: u32,
    /// Start the controls dump this many ms before the last key (0..5000, default 0), for a
    /// screen that goes away a few seconds after the key.
    #[serde(default)]
    dump_lead_ms: u32,
    /// Most controls to keep (1..5000, default 500).
    #[serde(default = "default_max_nodes")]
    max_nodes: usize,
    /// Absolute host folder that must not exist yet; it is created fresh.
    output_directory: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CrashLogParams {
    serial: String,
    /// Package name (e.g. com.example.app).
    package: String,
    /// Number of recent crash-log lines to read (1..10000, default 500).
    #[serde(default = "default_count")]
    count: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WalkStep {
    /// A short name for the step (its folder is `NN-<name>`); the keys when left out.
    #[serde(default)]
    name: Option<String>,
    /// Keys to press, as adb_keys_press takes them; none to only read the screen.
    #[serde(default)]
    keys: Vec<String>,
    /// Milliseconds between presses (0..10000, default 300).
    #[serde(default = "default_gap_ms")]
    gap_ms: u32,
    /// Milliseconds to wait after the last press before reading the screen (0..60000, default 1000).
    #[serde(default = "default_wait_ms")]
    wait_ms: u32,
    /// Start the controls dump this many ms before the last key (0..5000, default 0).
    #[serde(default)]
    dump_lead_ms: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WalkthroughParams {
    serial: String,
    /// The steps, in order (1..40).
    steps: Vec<WalkStep>,
    /// Most controls to keep per step (1..5000, default 500).
    #[serde(default = "default_max_nodes")]
    max_nodes: usize,
    /// Absolute host folder that must not exist yet; it is created fresh.
    output_directory: String,
}

fn default_max_states() -> usize {
    20
}

fn default_max_presses() -> u32 {
    400
}

fn default_time_budget_s() -> u32 {
    1800
}

fn default_launch_wait_ms() -> u32 {
    4000
}

fn default_map_wait_ms() -> u32 {
    1500
}

fn default_map_keys() -> Vec<String> {
    ["DPAD_UP", "DPAD_DOWN", "DPAD_LEFT", "DPAD_RIGHT", "DPAD_CENTER", "BACK", "MENU"].iter().map(|s| s.to_string()).collect()
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MapParams {
    serial: String,
    /// Package name (e.g. com.example.app).
    package: String,
    /// Which home-screen entry starts it: `launcher` (default, with a TV's entry as fallback) or `leanback`.
    #[serde(default)]
    category: LaunchCategory,
    /// Most screens to keep (1..60, default 20).
    #[serde(default = "default_max_states")]
    max_states: usize,
    /// Most key presses in all, counting the replayed paths (1..5000, default 400).
    #[serde(default = "default_max_presses")]
    max_presses: u32,
    /// Seconds the map may take (60..7200, default 1800).
    #[serde(default = "default_time_budget_s")]
    time_budget_s: u32,
    /// The keys pressed on every screen, one key name each (default the D-pad, OK, BACK and MENU).
    #[serde(default = "default_map_keys")]
    keys: Vec<String>,
    /// Texts of screens to leave alone (case-insensitive): such a screen is a row with no details.
    #[serde(default)]
    skip_texts: Vec<String>,
    /// Milliseconds to wait after each launch before pressing keys (0..60000, default 4000).
    #[serde(default = "default_launch_wait_ms")]
    launch_wait_ms: u32,
    /// Milliseconds between presses (0..10000, default 300).
    #[serde(default = "default_gap_ms")]
    gap_ms: u32,
    /// Milliseconds to wait after the last press before reading the screen (0..60000, default 1500).
    #[serde(default = "default_map_wait_ms")]
    wait_ms: u32,
    /// Start the controls dump this many ms before the last key (0..5000, default 0).
    #[serde(default)]
    dump_lead_ms: u32,
    /// Most controls to keep per screen (1..5000, default 500).
    #[serde(default = "default_max_nodes")]
    max_nodes: usize,
    /// Most focus positions walked on one screen (1..30, default 8): from each, the keys that move
    /// the focus and OK are pressed, so the screen a control opens is found.
    #[serde(default = "default_max_focus")]
    max_focus_per_screen: usize,
    /// Absolute host folder that must not exist yet; it is created fresh.
    output_directory: String,
}

fn default_max_focus() -> usize {
    8
}

/// Where one capture of the map landed.
enum Arrived {
    /// Another package was on screen: the key left the app.
    Left,
    /// A screen seen before; `new_focus` when the focus sits somewhere new on it.
    Seen { id: String, new_focus: bool },
    /// A screen not seen before, kept.
    New(String),
}

// ------------------------------------------------------------------------------------------------
// Shared plumbing

/// Build an adb handle for the active settings, wiring cancellation and progress-friendly deadlines.
fn adb_for(ctx: &Ctx<'_>) -> Result<Adb, ToolError> {
    let s = &ctx.session.settings;
    let path = craft_anything_engines::locate("adb", &s.engines.tools, &s.engine_store_dir()).ok_or_else(|| {
        ToolError::new(ErrorCode::EngineMissing, "Android platform-tools (adb) isn't installed.")
            .with_remediation("Install it from Settings → Engines, or run engines_install with engine \"adb\".")
    })?;
    Ok(Adb::new(path).cancel(cancel_of(ctx)))
}

fn cancel_of(ctx: &Ctx<'_>) -> Cancel {
    let c = ctx.cancel.clone();
    Arc::new(move || c.is_cancelled())
}

fn device_env(ctx: &Ctx<'_>) -> Environment {
    Environment {
        clean_room: ctx.session.clean_room(),
        engine: "adb".into(),
        engine_version: None,
        host: craft_anything_evidence::host_triple(),
        language_id: None,
    }
}

fn device_subject(serial: &str) -> Subject {
    Subject { kind: "device".into(), sha256: None, name: Some(serial.to_string()) }
}

/// Start a device evidence record on the authority of the external service (the device).
fn dev_ev(ctx: &Ctx<'_>, op: &str, serial: &str, params: Value) -> EvidenceBuilder {
    EvidenceBuilder::new(op, device_subject(serial), device_env(ctx)).params(params).authority(Authority::OutsideService).confidence(Confidence::Observed)
}

/// Confirm the serial names a device that is present and online; otherwise a clear, never-hanging
/// error.
fn ensure_online(adb: &Adb, serial: &str) -> Result<Device, ToolError> {
    let devices = adb.devices()?;
    match devices.iter().find(|d| d.serial == serial) {
        Some(d) if d.is_online() => Ok(d.clone()),
        Some(d) if d.state == "unauthorized" => Err(ToolError::new(ErrorCode::NoPermission, format!("Device {serial} has not authorised this computer."))
            .with_remediation("On the phone, accept the \"Allow USB debugging\" prompt, then try again.")),
        Some(d) => Err(ToolError::new(ErrorCode::NotFound, format!("Device {serial} is {}, not ready.", d.state))
            .with_remediation("Wait for it to come online (unlock it / reconnect it), then try again.")),
        None => {
            let serials: Vec<&str> = devices.iter().map(|d| d.serial.as_str()).collect();
            Err(ToolError::new(ErrorCode::NotFound, format!("No device with serial {serial} is connected."))
                .with_remediation("List connected devices with adb_devices_find and use one of their serials.")
                .with_details(json!({"connected": serials})))
        }
    }
}

/// stdout of an `adb shell` command, or an error when nothing usable came back.
fn shell_text(adb: &Adb, serial: &str, line: &str, what: &str) -> Result<String, ToolError> {
    let out = adb.shell(serial, line)?;
    usable(out, what)
}

fn usable(out: Output, what: &str) -> Result<String, ToolError> {
    if out.success || !out.stdout.is_empty() {
        return Ok(out.stdout_text());
    }
    Err(exec_error(what, &out))
}

/// Build the "command produced nothing usable" error from a finished run.
fn exec_error(what: &str, out: &Output) -> ToolError {
    let tail = tail_text(&out.stderr_text());
    let msg = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output").trim().to_string();
    ToolError::new(ErrorCode::ProgramFailed, format!("{what} failed: {msg}"))
        .with_remediation(format!("What adb printed last:\n{}", craft_anything_platform::process::output_excerpt(&tail)))
        .with_details(json!({"exit_code": out.code, "stderr_tail": tail}))
}

fn tail_text(s: &str) -> String {
    let n = s.chars().count();
    s.chars().skip(n.saturating_sub(2000)).collect()
}

/// The validated serial from params.
fn serial_of(s: &str) -> Result<&str, ToolError> {
    adb::validate_serial(s)
}

fn done(e: Evidence) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::evidence(e))
}

/// A limitation noting that large text output is summarised, with a bounded raw tail attached.
fn passthrough_limitation(cmd: &str) -> Limitation {
    Limitation::new(
        "dumpsys_passthrough",
        format!("Only key fields of `{cmd}` are parsed; the rest is attached as a bounded raw tail (first ~16 KiB), not structured."),
    )
}

/// Keep at most `max` bytes of text (for raw-tail attachments).
fn clip(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        (s.to_string(), false)
    } else {
        let cut = (0..=max).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        (s.get(..cut).unwrap_or("").to_string(), true)
    }
}

// ------------------------------------------------------------------------------------------------
// Tools

fn engines_adb_check(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let _: Empty = parse_params(params)?;
    let s = &ctx.session.settings;
    let found = craft_anything_engines::locate("adb", &s.engines.tools, &s.engine_store_dir());
    let mut lims = Vec::new();
    let (version, error) = match &found {
        Some(path) => match Adb::new(path.clone()).cancel(cancel_of(ctx)).timeout(Duration::from_secs(20)).version() {
            Ok(v) if !v.is_empty() => (Some(v), None),
            Ok(_) => (None, Some("adb did not report a version".to_string())),
            Err(e) => (None, Some(e.message)),
        },
        None => (None, None),
    };
    if let Some(e) = &error {
        lims.push(Limitation::new("version_probe_failed", format!("adb was found but reporting its version failed: {e}")));
    }
    let available = found.is_some() && error.is_none();
    let summary = match (&found, &version) {
        (Some(p), Some(v)) => format!("adb is available: {v} at {}.", p.display()),
        (Some(p), None) => format!("adb was found at {} but did not report a version.", p.display()),
        (None, _) => "adb is not installed. Install it from Settings → Engines, or run engines_install with engine \"adb\".".into(),
    };
    let e = EvidenceBuilder::new("engines_adb_check", Subject { kind: "engine".into(), sha256: None, name: Some("adb".into()) }, device_env(ctx))
        .params(json!({}))
        .authority(Authority::OurRun)
        .confidence(Confidence::Observed)
        .result(json!({
            "available": available,
            "path": found,
            "version": version,
            "error": error,
            "remediation": if available { None } else { Some("Install Android platform-tools, or run engines_install with engine \"adb\".") },
        }))
        .summary(summary)
        .limitations(lims)
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_devices_find(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let _: Empty = parse_params(params)?;
    let adb = adb_for(ctx)?;
    let devices = adb.devices()?;
    let online = devices.iter().filter(|d| d.is_online()).count();
    let summary = format!("{} device(s) attached, {online} online.", devices.len());
    let locations: Vec<Location> = devices.iter().map(|d| Location::new("device", &d.serial).with_label(d.state.clone())).collect();
    let e = EvidenceBuilder::new("adb_devices_find", Subject { kind: "engine".into(), sha256: None, name: Some("adb".into()) }, device_env(ctx))
        .params(json!({}))
        .authority(Authority::OutsideService)
        .confidence(Confidence::Observed)
        .result(json!({"devices": devices, "count": devices.len(), "online": online}))
        .summary(summary)
        .locations(locations)
        .coverage(Coverage::complete(devices.len() as u64))
        .build()?;
    done(e)
}

fn adb_device_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SerialParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, "getprop", "getprop")?;
    let props = adb::parse_getprop(&text);
    let mut whitelist = serde_json::Map::new();
    for key in adb::DEVICE_PROP_WHITELIST {
        if let Some(prop) = props.iter().find(|pr| pr.key == *key) {
            whitelist.insert((*key).to_string(), Value::String(prop.value.clone()));
        }
    }
    let model = whitelist.get("ro.product.model").and_then(Value::as_str).unwrap_or("?");
    let release = whitelist.get("ro.build.version.release").and_then(Value::as_str).unwrap_or("?");
    let sdk = whitelist.get("ro.build.version.sdk").and_then(Value::as_str).unwrap_or("?");
    // A TV (its home screen is the leanback launcher), and the display's density and size, so
    // positions from the screen's controls can be read in dp. Each is best effort: a device that
    // won't answer leaves the field null.
    let features = shell_text(&adb, serial, "pm list features", "pm list features").map(|t| adb::parse_features(&t)).unwrap_or_default();
    let tv = features.iter().any(|f| f == "android.software.leanback");
    let density = density_of(&adb, serial);
    let display = shell_text(&adb, serial, "wm size", "wm size").ok().and_then(|t| adb_ui::parse_wm_size(&t));
    let kind = if tv { "a TV" } else { "a phone or tablet" };
    let dpi = density.map(|d| format!(", {d} dpi")).unwrap_or_default();
    let summary = format!("{model}, Android {release} (API {sdk}), {kind}{dpi}; {} properties read.", props.len());
    // Attach a bounded sample of all properties so nothing silently disappears.
    let sample: Vec<_> = props.iter().take(2000).collect();
    let e = dev_ev(ctx, "adb_device_show", serial, json!({"serial": serial}))
        .result(json!({
            "device": whitelist,
            "tv": tv,
            "density_dpi": density,
            "display_px": display.map(|(w, h)| json!({"width": w, "height": h})),
            "property_count": props.len(),
            "properties": sample,
        }))
        .summary(summary)
        .coverage(Coverage::complete(props.len() as u64))
        .build()?;
    done(e)
}

fn adb_apps_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ListPackagesParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let flag = match p.scope {
        PackageScope::All => "",
        PackageScope::ThirdParty => " -3",
        PackageScope::System => " -s",
    };
    let text = shell_text(&adb, serial, &format!("pm list packages{flag}"), "pm list packages")?;
    let pkgs = adb::parse_packages(&text);
    let e = dev_ev(ctx, "adb_apps_list", serial, json!({"serial": serial, "scope": p.scope}))
        .result(json!({"packages": pkgs, "count": pkgs.len(), "scope": p.scope}))
        .summary(format!("{} package(s).", pkgs.len()))
        .coverage(Coverage::complete(pkgs.len() as u64))
        .build()?;
    done(e)
}

fn adb_log_read(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: LogcatParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    if !(1..=10000).contains(&p.count) {
        return Err(ToolError::invalid("count must be between 1 and 10000"));
    }
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    // Build host-side argv (no device shell): logcat is an adb subcommand.
    let mut args: Vec<String> = vec![
        "-s".into(),
        serial.into(),
        "logcat".into(),
        "-d".into(),
        "-b".into(),
        p.buffer.name().into(),
        "-v".into(),
        "threadtime".into(),
        "-t".into(),
        p.count.to_string(),
    ];
    if let Some(pid) = p.pid {
        args.push("--pid".into());
        args.push(pid.to_string());
    }
    let out = adb.run(&args)?;
    let text = usable(out, "logcat")?;
    let lines = adb::parse_logcat(&text);
    let e = dev_ev(ctx, "adb_log_read", serial, json!({"serial": serial, "count": p.count, "buffer": p.buffer, "pid": p.pid}))
        .result(json!({"lines": lines, "count": lines.len(), "buffer": p.buffer.name(), "pid": p.pid}))
        .summary(format!("{} log line(s) from the {} buffer.", lines.len(), p.buffer.name()))
        .coverage(Coverage::complete(lines.len() as u64))
        .build()?;
    done(e)
}

/// Whether `dumpsys package <name>` describes `package`: it has that package's `Package [name]`
/// block. Android answers a missing one with "Unable to find package: <name>", which names it too (#235).
fn package_found(text: &str, package: &str) -> bool {
    text.contains(&format!("Package [{package}]"))
}

fn adb_app_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PackageParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, &format!("dumpsys package {package}"), "dumpsys package")?;
    if !package_found(&text, package) {
        return Err(ToolError::new(ErrorCode::NotFound, format!("Package {package} is not installed on {serial}."))
            .with_remediation("List installed packages with adb_apps_list."));
    }
    let fields = grep_fields(
        &text,
        &[
            "versionName=",
            "versionCode=",
            "userId=",
            "firstInstallTime=",
            "lastUpdateTime=",
            "dataDir=",
            "codePath=",
            "primaryCpuAbi=",
            "targetSdk=",
            "minSdk=",
        ],
    );
    let perms = grep_section(&text, "requested permissions:", 400);
    let granted = grep_section(&text, "install permissions:", 400);
    let (raw, truncated) = clip(&text, 16 * 1024);
    let e = dev_ev(ctx, "adb_app_show", serial, json!({"serial": serial, "package": package}))
        .result(json!({
            "package": package,
            "fields": fields,
            "requested_permissions": perms,
            "install_permissions": granted,
            "raw_tail": raw,
            "raw_truncated": truncated,
        }))
        .summary(format!("Package {package}: {} field(s) parsed.", fields.len()))
        .limitation(passthrough_limitation("dumpsys package"))
        .location(Location::new("package", package))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_running_processes_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SerialParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, "ps -A", "ps -A")?;
    let rows = adb::parse_ps(&text);
    let e = dev_ev(ctx, "adb_running_processes_list", serial, json!({"serial": serial}))
        .result(json!({"processes": rows, "count": rows.len()}))
        .summary(format!("{} process(es).", rows.len()))
        .coverage(Coverage::complete(rows.len() as u64))
        .build()?;
    done(e)
}

fn adb_folder_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: DirectoryParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let path = adb::validate_device_path(&p.device_path)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let line = format!("ls -la {}", adb::quote_device(path));
    let out = adb.shell(serial, &line)?;
    let text = out.stdout_text();
    let entries = parse_ls(&text);
    let (raw, truncated) = clip(&text, 16 * 1024);
    let summary = if out.success || !entries.is_empty() {
        format!("{} entries in {path}.", entries.len())
    } else {
        format!("Could not list {path} (it may not exist or be readable).")
    };
    let e = dev_ev(ctx, "adb_folder_list", serial, json!({"serial": serial, "device_path": path}))
        .result(json!({"path": path, "entries": entries, "count": entries.len(), "raw_tail": raw, "raw_truncated": truncated, "exit_code": out.code}))
        .summary(summary)
        .location(Location::new("path", path))
        .coverage(Coverage::complete(entries.len() as u64))
        .build()?;
    done(e)
}

fn adb_device_features_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SerialParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, "pm list features", "pm list features")?;
    let feats = adb::parse_features(&text);
    let e = dev_ev(ctx, "adb_device_features_list", serial, json!({"serial": serial}))
        .result(json!({"features": feats, "count": feats.len()}))
        .summary(format!("{} feature(s).", feats.len()))
        .coverage(Coverage::complete(feats.len() as u64))
        .build()?;
    done(e)
}

fn adb_system_services_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SerialParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, "service list", "service list")?;
    let svcs = adb::parse_services(&text);
    let e = dev_ev(ctx, "adb_system_services_list", serial, json!({"serial": serial}))
        .result(json!({"services": svcs, "count": svcs.len()}))
        .summary(format!("{} system service(s).", svcs.len()))
        .coverage(Coverage::complete(svcs.len() as u64))
        .build()?;
    done(e)
}

fn adb_display_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    dumpsys_tool(ctx, params, "display", &["mDisplayId", "DisplayDeviceInfo", "real ", "density", "mState", "FLAG_"])
}

fn adb_window_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    dumpsys_tool(ctx, params, "window", &["mCurrentFocus", "mFocusedApp", "mDisplayFrames", "imeLayeringTarget", "mInputMethodTarget"])
}

fn dumpsys_tool(ctx: &mut Ctx<'_>, params: Value, what: &'static str, needles: &[&str]) -> Result<ToolOutput, ToolError> {
    let p: SerialParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, &format!("dumpsys {what}"), &format!("dumpsys {what}"))?;
    let key_lines = grep_lines(&text, needles, 200);
    let (raw, truncated) = clip(&text, 16 * 1024);
    let op = if what == "display" { "adb_display_show" } else { "adb_window_show" };
    let e = dev_ev(ctx, op, serial, json!({"serial": serial}))
        .result(json!({"key_lines": key_lines, "raw_tail": raw, "raw_truncated": truncated}))
        .summary(format!("dumpsys {what}: {} key line(s).", key_lines.len()))
        .limitation(passthrough_limitation(&format!("dumpsys {what}")))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_setting_get(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SettingParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let key = adb::validate_dotted_key(&p.key, "setting key")?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let ns = p.namespace.name();
    let text = shell_text(&adb, serial, &format!("settings get {ns} {key}"), "settings get")?;
    let value = text.trim();
    let value = if value == "null" { Value::Null } else { Value::String(value.to_string()) };
    let e = dev_ev(ctx, "adb_setting_get", serial, json!({"serial": serial, "namespace": ns, "key": key}))
        .result(json!({"namespace": ns, "key": key, "value": value}))
        .summary(format!("{ns}/{key} = {value}"))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_apps_find(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ResolveParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let query = adb::validate_query(&p.query)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let text = shell_text(&adb, serial, &format!("pm list packages {}", adb::quote_device(query)), "pm list packages")?;
    let pkgs = adb::parse_packages(&text);
    let e = dev_ev(ctx, "adb_apps_find", serial, json!({"serial": serial, "query": query}))
        .result(json!({"query": query, "packages": pkgs, "count": pkgs.len()}))
        .summary(format!("{} package(s) match {query:?}.", pkgs.len()))
        .coverage(Coverage::complete(pkgs.len() as u64))
        .build()?;
    done(e)
}

fn adb_app_download(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PullPackageParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(300)).idle_timeout(Duration::from_secs(120));
    ensure_online(&adb, serial)?;
    let path_text = shell_text(&adb, serial, &format!("pm path {package}"), "pm path")?;
    let device_paths: Vec<String> = path_text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .map(|s| s.trim().to_string())
        .filter(|s| adb::validate_device_path(s).is_ok())
        .collect();
    if device_paths.is_empty() {
        return Err(ToolError::new(ErrorCode::NotFound, format!("Could not find APK paths for {package} on {serial}."))
            .with_remediation("Check the package is installed with adb_apps_list."));
    }
    let mut pulled = Vec::new();
    for dp in &device_paths {
        ctx.cancel.check()?;
        let out = adb.targeted(serial, ["pull", dp.as_str(), &outdir.to_string_lossy()])?;
        if !out.success {
            return Err(pull_error(dp, &out));
        }
        pulled.push(dp.clone());
    }
    let written = list_dir_names(&outdir);
    let e = dev_ev(ctx, "adb_app_download", serial, json!({"serial": serial, "package": package, "output_directory": outdir.to_string_lossy()}))
        .result(json!({"package": package, "device_paths": device_paths, "pulled": pulled, "output_directory": outdir, "files": written}))
        .summary(format!("Pulled {} APK file(s) for {package} to {}.", pulled.len(), outdir.display()))
        .authority(Authority::OutsideService)
        .location(Location::new("path", outdir.to_string_lossy()))
        .limitation(Limitation::new(
            "not_verified",
            "The pulled APKs are copied as-is; their signatures are not verified by this tool (use android_package_show).",
        ))
        .coverage(Coverage::complete(pulled.len() as u64))
        .build()?;
    made.keep();
    done(e)
}

pub(crate) fn pull_error(path: &str, out: &Output) -> ToolError {
    ToolError::new(ErrorCode::ProgramFailed, format!("adb pull of {path} failed: {}", tail_text(&out.stderr_text()).trim())).with_remediation(format!(
        "Check the device path exists and is readable. To try it yourself, run:\nadb pull {}",
        craft_anything_platform::process::printable_command(std::ffi::OsStr::new(path), &[] as &[&str])
    ))
}

fn adb_file_download(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PullFileParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let path = adb::validate_device_path(&p.device_path)?;
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(300)).idle_timeout(Duration::from_secs(120));
    ensure_online(&adb, serial)?;
    let out = adb.targeted(serial, ["pull", path, &outdir.to_string_lossy()])?;
    if !out.success {
        return Err(pull_error(path, &out));
    }
    let files = list_dir_names(&outdir);
    let e = dev_ev(ctx, "adb_file_download", serial, json!({"serial": serial, "device_path": path, "output_directory": outdir.to_string_lossy()}))
        .result(json!({"device_path": path, "output_directory": outdir, "files": files}))
        .summary(format!("Pulled {path} to {}.", outdir.display()))
        .authority(Authority::OutsideService)
        .location(Location::new("path", outdir.to_string_lossy()))
        .coverage(Coverage::complete(files.len() as u64))
        .build()?;
    made.keep();
    done(e)
}

fn adb_screenshot_take(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: OutputDirParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(60));
    ensure_online(&adb, serial)?;
    let out = adb.exec_out(serial, "screencap -p")?;
    if !out.success && out.stdout.is_empty() {
        return Err(exec_error("screencap", &out));
    }
    if out.stdout.len() < 8 || out.stdout.get(1..4) != Some(b"PNG") {
        return Err(ToolError::new(ErrorCode::GarbledOutput, "The screenshot did not come back as a PNG.")
            .with_remediation("Some devices block screen capture; try again or capture it on the device."));
    }
    let file = outdir.join("screen.png");
    write_atomic(&file, &out.stdout)?;
    let e = dev_ev(ctx, "adb_screenshot_take", serial, json!({"serial": serial, "output_directory": outdir.to_string_lossy()}))
        .result(json!({"file": file, "bytes": out.stdout.len()}))
        .summary(format!("Saved a {}-byte screenshot to {}.", out.stdout.len(), file.display()))
        .authority(Authority::OutsideService)
        .location(Location::new("path", file.to_string_lossy()))
        .coverage(Coverage::complete(1))
        .build()?;
    made.keep();
    done(e)
}

fn adb_bugreport_save(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: OutputDirParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    // Bug reports can take minutes; keep a generous wall deadline but an idle deadline so a truly
    // stuck run still ends.
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(600)).idle_timeout(Duration::from_secs(180));
    ensure_online(&adb, serial)?;
    ctx.report(None, "collecting a bug report (this can take a few minutes)");
    let out = adb.targeted(serial, ["bugreport", &outdir.to_string_lossy()])?;
    let files = list_dir_names(&outdir);
    if !out.success && files.is_empty() {
        return Err(exec_error("bugreport", &out));
    }
    let e = dev_ev(ctx, "adb_bugreport_save", serial, json!({"serial": serial, "output_directory": outdir.to_string_lossy()}))
        .result(json!({"output_directory": outdir, "files": files, "stdout_tail": tail_text(&out.stdout_text())}))
        .summary(format!("Collected a bug report ({} file(s)) in {}.", files.len(), outdir.display()))
        .authority(Authority::OutsideService)
        .location(Location::new("path", outdir.to_string_lossy()))
        .coverage(Coverage::complete(files.len() as u64))
        .build()?;
    made.keep();
    done(e)
}

fn adb_file_upload(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PushParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let local = super::artifact_tools::abs_path(&p.local_path, "local_path")?;
    if !local.is_file() {
        return Err(ToolError::invalid(format!("{} is not a file", local.display())));
    }
    let device_path = adb::validate_device_path(&p.device_path)?;
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(300)).idle_timeout(Duration::from_secs(120));
    ensure_online(&adb, serial)?;
    if !p.overwrite {
        // `test -e PATH && echo EXISTS`
        let probe = shell_text(&adb, serial, &format!("( [ -e {} ] && echo EXISTS ) || echo ABSENT", adb::quote_device(device_path)), "test -e")?;
        if probe.contains("EXISTS") {
            return Err(ToolError::new(ErrorCode::BadInput, format!("{device_path} already exists on the device."))
                .with_remediation("Pass overwrite=true to replace it, or choose a different device path."));
        }
    }
    let out = adb.targeted(serial, ["push", &local.to_string_lossy(), device_path])?;
    if !out.success {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("adb push failed: {}", tail_text(&out.stderr_text()).trim()))
            .with_remediation("Check the destination folder exists and is writable on the device."));
    }
    let e = dev_ev(
        ctx,
        "adb_file_upload",
        serial,
        json!({"serial": serial, "local_path": local.to_string_lossy(), "device_path": device_path, "overwrite": p.overwrite}),
    )
    .result(json!({"local_path": local, "device_path": device_path, "stdout_tail": tail_text(&out.stdout_text())}))
    .summary(format!("Pushed {} to {device_path}.", local.display()))
    .limitation(Limitation::new("device_changed", "This wrote a file onto the device."))
    .location(Location::new("path", device_path))
    .coverage(Coverage::complete(1))
    .build()?;
    done(e)
}

fn adb_app_install(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: InstallParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let apk = super::artifact_tools::abs_path(&p.apk_path, "apk_path")?;
    if !apk.is_file() {
        return Err(ToolError::invalid(format!("{} is not a file", apk.display())));
    }
    let adb = adb_for(ctx)?.timeout(Duration::from_secs(300)).idle_timeout(Duration::from_secs(120));
    ensure_online(&adb, serial)?;
    let apk_s = apk.to_string_lossy().into_owned();
    let args: Vec<&str> = if p.replace { vec!["install", "-r", &apk_s] } else { vec!["install", &apk_s] };
    let out = adb.targeted(serial, args)?;
    let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
    if !out.success || !combined.contains("Success") {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("adb install failed: {}", tail_text(&combined).trim()))
            .with_remediation("The APK may be incompatible, unsigned for this device, or already installed; see the message."));
    }
    let e = dev_ev(ctx, "adb_app_install", serial, json!({"serial": serial, "apk_path": apk_s, "replace": p.replace}))
        .result(json!({"apk_path": apk, "replace": p.replace, "output": tail_text(&combined)}))
        .summary(format!("Installed {}.", apk.display()))
        .limitation(Limitation::new("device_changed", "This installed an app onto the device."))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_app_remove(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PackageParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let out = adb.targeted(serial, ["uninstall", package])?;
    let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
    if !out.success || !combined.contains("Success") {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("adb uninstall failed: {}", tail_text(&combined).trim()))
            .with_remediation("The app may not be installed, or may be a protected system app."));
    }
    let e = dev_ev(ctx, "adb_app_remove", serial, json!({"serial": serial, "package": package}))
        .result(json!({"package": package, "output": tail_text(&combined)}))
        .summary(format!("Uninstalled {package}."))
        .limitation(Limitation::new("device_changed", "This removed an app from the device."))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_app_launch(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: LaunchParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let (category, tried) = launch_app(&adb, serial, package, p.category)?;
    let entry = match category {
        LaunchCategory::Launcher => "launcher",
        LaunchCategory::Leanback => "TV (leanback)",
    };
    let e = dev_ev(ctx, "adb_app_launch", serial, json!({"serial": serial, "package": package, "category": p.category}))
        .result(json!({"package": package, "category": category, "intent": category.intent(), "tried": tried}))
        .summary(format!("Launched {package} by its {entry} entry."))
        .limitation(Limitation::new("device_changed", "This started an app on the device."))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

fn adb_activity_open(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ActivityParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    if p.extras.len() > 16 {
        return Err(ToolError::invalid("at most 16 extras are allowed"));
    }
    if p.action.is_none() && p.component.is_none() {
        return Err(ToolError::invalid("pass at least one of action or component"));
    }
    let mut line = String::from("am start -W");
    if let Some(a) = &p.action {
        let a = adb::validate_dotted_key(a, "intent action")?;
        line.push_str(&format!(" -a {a}"));
    }
    if let Some(c) = &p.component {
        let c = adb::validate_component(c)?;
        line.push_str(&format!(" -n {c}"));
    }
    if let Some(d) = &p.data_uri {
        if d.len() > 4096 || d.bytes().any(|b| b == 0 || b == b'\n' || b == b'\r') {
            return Err(ToolError::invalid("data_uri is too long or has control characters"));
        }
        line.push_str(&format!(" -d {}", adb::quote_device(d)));
    }
    for ex in &p.extras {
        let key = adb::validate_dotted_key(&ex.key, "extra key")?;
        let frag = extra_fragment(key, ex.ty, &ex.value)?;
        line.push(' ');
        line.push_str(&frag);
    }
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let out = adb.shell(serial, &line)?;
    let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
    if combined.contains("Error:") || combined.contains("Exception") {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("am start failed: {}", tail_text(&combined).trim()))
            .with_remediation("Check the action, component and extras are valid for the target app."));
    }
    let e = dev_ev(
        ctx,
        "adb_activity_open",
        serial,
        json!({"serial": serial, "action": p.action, "component": p.component, "data_uri": p.data_uri, "extras_count": p.extras.len()}),
    )
    .result(json!({"output": tail_text(&combined)}))
    .summary("Started the activity.".to_string())
    .limitation(Limitation::new("device_changed", "This started an activity on the device."))
    .coverage(Coverage::complete(1))
    .build()?;
    done(e)
}

fn extra_fragment(key: &str, ty: ExtraType, value: &str) -> Result<String, ToolError> {
    Ok(match ty {
        ExtraType::String => format!("--es {key} {}", adb::quote_device(value)),
        ExtraType::Boolean => {
            let b: bool = value.parse().map_err(|_| ToolError::invalid(format!("extra {key}: expected true/false, got {value:?}")))?;
            format!("--ez {key} {b}")
        }
        ExtraType::Int => {
            let n: i32 = value.parse().map_err(|_| ToolError::invalid(format!("extra {key}: expected an int, got {value:?}")))?;
            format!("--ei {key} {n}")
        }
        ExtraType::Long => {
            let n: i64 = value.parse().map_err(|_| ToolError::invalid(format!("extra {key}: expected a long, got {value:?}")))?;
            format!("--el {key} {n}")
        }
        ExtraType::Float => {
            let n: f64 = value.parse().map_err(|_| ToolError::invalid(format!("extra {key}: expected a float, got {value:?}")))?;
            if !n.is_finite() {
                return Err(ToolError::invalid(format!("extra {key}: value must be finite")));
            }
            format!("--ef {key} {n}")
        }
    })
}

fn adb_app_force_stop(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PackageParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let out = adb.shell(serial, &format!("am force-stop {package}"))?;
    let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
    if combined.contains("Error") || combined.contains("Exception") {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("am force-stop failed: {}", tail_text(&combined).trim())));
    }
    let e = dev_ev(ctx, "adb_app_force_stop", serial, json!({"serial": serial, "package": package}))
        .result(json!({"package": package}))
        .summary(format!("Force-stopped {package}."))
        .limitation(Limitation::new("device_changed", "This stopped an app on the device."))
        .coverage(Coverage::complete(1))
        .build()?;
    done(e)
}

// ------------------------------------------------------------------------------------------------
// Light parsers for text that is otherwise passed through

/// Pull `name=value` fields out of dumpsys-style text (first occurrence of each).
fn grep_fields(text: &str, keys: &[&str]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for line in text.lines().take(adb::MAX_LINES) {
        let line = line.trim();
        for key in keys {
            if out.contains_key(key.trim_end_matches('=')) {
                continue;
            }
            if let Some(idx) = line.find(key) {
                let after = line.get(idx + key.len()..).unwrap_or("");
                let value = after.split_whitespace().next().unwrap_or("").to_string();
                if !value.is_empty() {
                    out.insert(key.trim_end_matches('=').to_string(), Value::String(value));
                }
            }
        }
    }
    out
}

/// Collect the indented lines of a named section (e.g. "requested permissions:").
fn grep_section(text: &str, header: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut base_indent = 0usize;
    for line in text.lines().take(adb::MAX_LINES) {
        let indent = line.len() - line.trim_start().len();
        if !in_section {
            if line.trim() == header || line.trim_start() == header {
                in_section = true;
                base_indent = indent;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if indent <= base_indent {
            break;
        }
        out.push(line.trim().to_string());
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Lines containing any of the needles (bounded).
fn grep_lines(text: &str, needles: &[&str], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().take(adb::MAX_LINES) {
        if needles.iter().any(|n| line.contains(n)) {
            out.push(line.trim().to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// A parsed `ls -la` entry (best effort; the raw text is always kept by the caller).
#[derive(Debug, Serialize)]
struct LsEntry {
    perms: String,
    size: Option<u64>,
    name: String,
}

fn parse_ls(text: &str) -> Vec<LsEntry> {
    let mut out = Vec::new();
    for line in text.lines().take(adb::MAX_LINES) {
        let line = line.trim();
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(perms) = fields.first() else { continue };
        if perms.len() < 10 || !matches!(perms.as_bytes().first(), Some(b'-' | b'd' | b'l' | b'c' | b'b' | b'p' | b's')) {
            continue;
        }
        // toybox `ls -la`: perms links owner group size month day time name...
        let size = fields.get(4).and_then(|s| s.parse::<u64>().ok());
        let name = fields.get(7..).map(|s| s.join(" ")).filter(|s| !s.is_empty()).unwrap_or_default();
        out.push(LsEntry { perms: (*perms).to_string(), size, name });
    }
    out
}

fn list_dir_names(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten().take(10_000) {
            out.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

// ------------------------------------------------------------------------------------------------
// The screen as a person sees and drives it (R2.12)

/// Where a controls dump goes on the device before it is read back: a name of this call's own,
/// removed in the same shell call.
fn dump_path(serial: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serial.hash(&mut h);
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    format!("/sdcard/.craft-ui-{:016x}.xml", h.finish())
}

/// The device-shell commands that dump the screen's controls to `path`, and that print and
/// remove the file.
fn dump_commands(path: &str) -> (String, String) {
    let q = adb::quote_device(path);
    (format!("uiautomator dump {q} >/dev/null 2>&1"), format!("cat {q}; rm -f {q}"))
}

/// Shell commands joined into one line: `;` between them, except after one sent to the
/// background with `&`, which needs no separator.
fn join_commands(cmds: &[String]) -> String {
    let mut line = String::new();
    for c in cmds {
        if !line.is_empty() {
            line.push_str(if line.ends_with('&') { " " } else { "; " });
        }
        line.push_str(c);
    }
    line
}

/// The display's density, when `wm density` answers.
fn density_of(adb: &Adb, serial: &str) -> Option<u32> {
    shell_text(adb, serial, "wm density", "wm density").ok().and_then(|t| adb_ui::parse_wm_density(&t))
}

fn check_at_most(v: u32, max: u32, what: &str) -> Result<(), ToolError> {
    if v > max { Err(ToolError::invalid(format!("{what} must be at most {max}, not {v}"))) } else { Ok(()) }
}

fn check_nodes(n: usize) -> Result<usize, ToolError> {
    if (1..=adb_ui::MAX_UI_NODES).contains(&n) { Ok(n) } else { Err(ToolError::invalid(format!("max_nodes must be between 1 and {}", adb_ui::MAX_UI_NODES))) }
}

/// An adb handle whose deadlines allow `busy_ms` of sleeping on the device (the idle deadline
/// would otherwise end a long key sequence that prints nothing).
fn adb_for_wait(ctx: &Ctx<'_>, busy_ms: u64) -> Result<Adb, ToolError> {
    let d = Duration::from_secs(90 + busy_ms / 1000);
    Ok(adb_for(ctx)?.timeout(d).idle_timeout(d))
}

/// The controls a dump describes; an empty answer is an error that says what to try.
fn nodes_of(xml: &str, max_nodes: usize) -> Result<Vec<adb_ui::UiNode>, ToolError> {
    let nodes = adb_ui::parse_ui_dump(xml, max_nodes);
    if nodes.is_empty() {
        return Err(ToolError::new(ErrorCode::GarbledOutput, "The screen's controls couldn't be read: uiautomator reported none.")
            .with_remediation("Make sure the screen is on and unlocked and that the app has drawn something; a protected (DRM) surface or a screen still loading can read as empty. Then try again."));
    }
    Ok(nodes)
}

fn write_json(file: &Path, value: &Value) -> Result<(), ToolError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| ToolError::internal(e.to_string()))?;
    write_atomic(file, text.as_bytes())
}

fn adb_keys_press(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: KeysParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let keys = adb_ui::parse_keys(&p.keys)?;
    check_at_most(p.gap_ms, 10_000, "gap_ms")?;
    check_at_most(p.wait_ms, 60_000, "wait_ms")?;
    let presses: u64 = keys.iter().map(|k| u64::from(k.times)).sum();
    let adb = adb_for_wait(ctx, presses * (u64::from(p.gap_ms) + 100) + u64::from(p.wait_ms))?;
    ensure_online(&adb, serial)?;
    let mut cmds = adb_ui::key_commands(&keys, p.gap_ms);
    if p.wait_ms > 0 {
        cmds.push(format!("sleep {}", adb_ui::seconds(p.wait_ms)));
    }
    let out = adb.shell(serial, &join_commands(&cmds))?;
    if !out.success {
        return Err(exec_error("input keyevent", &out));
    }
    let e = dev_ev(ctx, "adb_keys_press", serial, json!({"serial": serial, "keys": p.keys, "gap_ms": p.gap_ms, "wait_ms": p.wait_ms}))
        .result(json!({"keys": keys, "presses": presses}))
        .summary(format!("Pressed {presses} key(s)."))
        .limitation(Limitation::new("device_changed", "This pressed keys on the device; the app on screen reacted to them."))
        .coverage(Coverage::complete(presses))
        .build()?;
    done(e)
}

fn adb_text_type(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: TextParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let text = adb_ui::validate_text(&p.text)?;
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let out = adb.shell(serial, &adb_ui::text_command(text))?;
    if !out.success {
        return Err(exec_error("input text", &out));
    }
    let chars = text.chars().count();
    let e = dev_ev(ctx, "adb_text_type", serial, json!({"serial": serial, "characters": chars}))
        .result(json!({"characters": chars}))
        .summary(format!("Typed {chars} character(s)."))
        .limitation(Limitation::new("device_changed", "This typed into the field on screen."))
        .coverage(Coverage::complete(chars as u64))
        .build()?;
    done(e)
}

fn adb_ui_read(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: UiReadParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let max_nodes = check_nodes(p.max_nodes)?;
    let outdir = p.output_directory.as_deref().map(create_absent_dir).transpose()?;
    let made = outdir.as_deref().map(UnlessWritten::new);
    let adb = adb_for_wait(ctx, 0)?;
    ensure_online(&adb, serial)?;
    let (dump, read) = dump_commands(&dump_path(serial));
    let xml = shell_text(&adb, serial, &format!("{dump}; {read}"), "uiautomator dump")?;
    let nodes = nodes_of(&xml, max_nodes)?;
    let density = density_of(&adb, serial);
    let package = nodes.iter().map(|n| n.package.as_str()).find(|p| !p.is_empty());
    let tree = adb_ui::ui_tree_json(&nodes, density, package);
    let mut result = tree.clone();
    if let Some(dir) = &outdir {
        let file = dir.join("ui.json");
        write_json(&file, &tree)?;
        result["file"] = json!(file);
    }
    let focus = tree["focused_title"].as_str().map(|t| format!(" Focus is on \"{t}\".")).unwrap_or_default();
    let mut e = dev_ev(ctx, "adb_ui_read", serial, json!({"serial": serial, "max_nodes": max_nodes, "output_directory": p.output_directory}))
        .result(result)
        .summary(format!("{} control(s) on screen.{focus}", nodes.len()))
        .coverage(Coverage::complete(nodes.len() as u64));
    if nodes.len() >= max_nodes {
        e = e.limitation(Limitation::new("truncated", format!("Only the first {max_nodes} controls were kept; raise max_nodes for the rest.")));
    }
    if density.is_none() {
        e = e.limitation(Limitation::new("no_density", "The display's density wasn't read, so positions are in pixels only."));
    }
    let e = e.build()?;
    if let Some(m) = made {
        m.keep();
    }
    done(e)
}

/// Start `package` by its home-screen entry: the category asked for and, for the default, a
/// TV's entry next, for an app that has only that. Returns the entry that worked and what was
/// tried.
fn launch_app(adb: &Adb, serial: &str, package: &str, category: LaunchCategory) -> Result<(LaunchCategory, Vec<Value>), ToolError> {
    let categories: &[LaunchCategory] = match category {
        LaunchCategory::Launcher => &[LaunchCategory::Launcher, LaunchCategory::Leanback],
        LaunchCategory::Leanback => &[LaunchCategory::Leanback],
    };
    let mut tried = Vec::new();
    let mut last_output = String::new();
    for category in categories {
        let out = adb.shell(serial, &launch_command(package, *category))?;
        let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
        let failed = combined.contains("No activities found") || combined.contains("Error");
        last_output = tail_text(&combined).trim().to_string();
        tried.push(json!({"category": category, "ok": !failed, "output": last_output}));
        if !failed {
            return Ok((*category, tried));
        }
    }
    Err(ToolError::new(ErrorCode::ProgramFailed, format!("Could not launch {package}: {last_output}"))
        .with_remediation("The app may have no launcher activity; use adb_activity_open with an explicit component.")
        .with_details(json!({"tried": tried})))
}

/// The device-shell command that starts a package by one home-screen entry.
fn launch_command(package: &str, category: LaunchCategory) -> String {
    format!("monkey -p {package} -c {} 1", category.intent())
}

/// What one capture of the screen does: commands to run first (a launch), the keys, the
/// timings and how much to keep.
struct CaptureSpec<'a> {
    /// Device-shell commands run before the keys (a force-stop and launch, then a wait).
    pre: Vec<String>,
    /// The keys as the person wrote them, for keys.txt.
    key_tokens: &'a [String],
    keys: Vec<adb_ui::KeyPress>,
    gap_ms: u32,
    wait_ms: u32,
    dump_lead_ms: u32,
    max_nodes: usize,
}

impl CaptureSpec<'_> {
    fn presses(&self) -> u64 {
        self.keys.iter().map(|k| u64::from(k.times)).sum()
    }

    /// Milliseconds the device will spend sleeping for this capture.
    fn busy_ms(&self) -> u64 {
        self.presses() * (u64::from(self.gap_ms) + 100) + u64::from(self.wait_ms) + u64::from(self.dump_lead_ms)
    }
}

/// What a capture left in its folder and what it saw.
struct StateFiles {
    ui_file: PathBuf,
    screen_file: Option<PathBuf>,
    keys_file: PathBuf,
    nodes: Vec<adb_ui::UiNode>,
    tree: Value,
    fingerprint: String,
    density: Option<u32>,
    with_lead: bool,
}

impl StateFiles {
    fn focused_title(&self) -> Option<String> {
        self.tree["focused_title"].as_str().map(str::to_string)
    }

    /// The files relative to `base`, for a listing.
    fn files_json(&self, base: &Path) -> Value {
        let rel = |p: &Path| p.strip_prefix(base).unwrap_or(p).to_string_lossy().to_string();
        json!({"ui": rel(&self.ui_file), "screen": self.screen_file.as_deref().map(rel), "keys": rel(&self.keys_file)})
    }
}

/// One capture of the screen into `outdir` (which exists and is empty): the `pre` commands, the
/// keys, the dump (led or after a wait) and the picture, all through one device-shell call plus
/// one for the picture. The shared body of adb_screen_state_capture, the walkthrough and the map.
fn capture_state(adb: &Adb, serial: &str, spec: &CaptureSpec<'_>, outdir: &Path) -> Result<StateFiles, ToolError> {
    let (dump, read) = dump_commands(&dump_path(serial));
    let key_cmds = adb_ui::key_commands(&spec.keys, spec.gap_ms);
    let mut cmds = spec.pre.clone();
    let with_lead = spec.dump_lead_ms > 0 && !key_cmds.is_empty();
    if with_lead {
        // The dump attaches to the screen about a second after it starts, then reads it once it
        // has been still for a moment: started `lead` before the last key, it reads the screen
        // right after that key, not seconds later. The other keys go first.
        let (head, last) = key_cmds.split_at(key_cmds.len().saturating_sub(1));
        cmds.extend(head.iter().cloned());
        cmds.push(format!("{dump} &"));
        cmds.push(format!("sleep {}", adb_ui::seconds(spec.dump_lead_ms)));
        cmds.extend(last.iter().cloned());
        cmds.push("wait".to_string());
    } else {
        cmds.extend(key_cmds);
        if spec.wait_ms > 0 {
            cmds.push(format!("sleep {}", adb_ui::seconds(spec.wait_ms)));
        }
        cmds.push(dump);
    }
    cmds.push(read);
    let xml = shell_text(adb, serial, &join_commands(&cmds), "uiautomator dump")?;
    let nodes = nodes_of(&xml, spec.max_nodes)?;
    let density = density_of(adb, serial);
    let package = nodes.iter().map(|n| n.package.as_str()).find(|p| !p.is_empty());
    let tree = adb_ui::ui_tree_json(&nodes, density, package);
    let ui_file = outdir.join("ui.json");
    write_json(&ui_file, &tree)?;
    let keys_file = outdir.join("keys.txt");
    write_atomic(&keys_file, spec.key_tokens.join("\n").as_bytes())?;
    // The picture, best effort: a screen that refuses capture still has its controls.
    let shot = adb.exec_out(serial, "screencap -p")?;
    let png = shot.stdout.len() >= 8 && shot.stdout.get(1..4) == Some(b"PNG");
    let screen_file = outdir.join("screen.png");
    if png {
        write_atomic(&screen_file, &shot.stdout)?;
    }
    let fingerprint = adb_ui::screen_fingerprint(&nodes);
    Ok(StateFiles { ui_file, screen_file: png.then_some(screen_file), keys_file, nodes, tree, fingerprint, density, with_lead })
}

fn adb_screen_state_capture(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: StateCaptureParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let keys = if p.keys.is_empty() { Vec::new() } else { adb_ui::parse_keys(&p.keys)? };
    check_at_most(p.gap_ms, 10_000, "gap_ms")?;
    check_at_most(p.wait_ms, 60_000, "wait_ms")?;
    check_at_most(p.dump_lead_ms, 5_000, "dump_lead_ms")?;
    let max_nodes = check_nodes(p.max_nodes)?;
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let spec = CaptureSpec { pre: Vec::new(), key_tokens: &p.keys, keys, gap_ms: p.gap_ms, wait_ms: p.wait_ms, dump_lead_ms: p.dump_lead_ms, max_nodes };
    let presses = spec.presses();
    let adb = adb_for_wait(ctx, spec.busy_ms())?;
    ensure_online(&adb, serial)?;
    let got = capture_state(&adb, serial, &spec, &outdir)?;
    let focus = got.focused_title().map(|t| format!(" Focus is on \"{t}\".")).unwrap_or_default();
    let mut e = dev_ev(
        ctx,
        "adb_screen_state_capture",
        serial,
        json!({"serial": serial, "keys": p.keys, "gap_ms": p.gap_ms, "wait_ms": p.wait_ms, "dump_lead_ms": p.dump_lead_ms, "max_nodes": max_nodes, "output_directory": outdir.to_string_lossy()}),
    )
    .result(json!({
        "ui_file": got.ui_file,
        "screen_file": got.screen_file,
        "keys_file": got.keys_file,
        "keys": spec.keys,
        "presses": presses,
        "count": got.nodes.len(),
        "focused": got.tree["focused"],
        "focused_title": got.tree["focused_title"],
        "fingerprint": got.fingerprint,
        "density_dpi": got.density,
        "dump_lead_used": got.with_lead,
    }))
    .summary(format!("Captured the screen after {presses} key(s): {} control(s).{focus}", got.nodes.len()))
    .location(Location::new("path", got.ui_file.to_string_lossy()))
    .coverage(Coverage::complete(got.nodes.len() as u64));
    if presses > 0 {
        e = e.limitation(Limitation::new("device_changed", "This pressed keys on the device; the app on screen reacted to them."));
    }
    if got.screen_file.is_none() {
        e = e.limitation(Limitation::new("no_picture", "The screenshot didn't come back as a PNG (some screens block capture), so screen.png wasn't written."));
    }
    if got.nodes.len() >= max_nodes {
        e = e.limitation(Limitation::new("truncated", format!("Only the first {max_nodes} controls were kept; raise max_nodes for the rest.")));
    }
    let e = e.build()?;
    made.keep();
    done(e)
}

fn adb_ui_walkthrough_record(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: WalkthroughParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    if p.steps.is_empty() || p.steps.len() > 40 {
        return Err(ToolError::invalid("steps must hold 1 to 40 entries"));
    }
    let max_nodes = check_nodes(p.max_nodes)?;
    // Every step is checked before the first key is pressed.
    let mut specs = Vec::with_capacity(p.steps.len());
    for (i, step) in p.steps.iter().enumerate() {
        let keys = if step.keys.is_empty() {
            Vec::new()
        } else {
            adb_ui::parse_keys(&step.keys).map_err(|e| ToolError::invalid(format!("step {}: {}", i + 1, e.message)))?
        };
        check_at_most(step.gap_ms, 10_000, "gap_ms")?;
        check_at_most(step.wait_ms, 60_000, "wait_ms")?;
        check_at_most(step.dump_lead_ms, 5_000, "dump_lead_ms")?;
        specs.push(CaptureSpec {
            pre: Vec::new(),
            key_tokens: &step.keys,
            keys,
            gap_ms: step.gap_ms,
            wait_ms: step.wait_ms,
            dump_lead_ms: step.dump_lead_ms,
            max_nodes,
        });
    }
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let busy: u64 = specs.iter().map(CaptureSpec::busy_ms).sum();
    let adb = adb_for_wait(ctx, busy)?;
    ensure_online(&adb, serial)?;
    let mut steps = Vec::new();
    let mut presses = 0u64;
    let mut pictures = 0usize;
    for (i, (step, spec)) in p.steps.iter().zip(&specs).enumerate() {
        ctx.cancel.check()?;
        let name =
            step.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| if step.keys.is_empty() { "look".to_string() } else { step.keys.join(" ") });
        let dir = outdir.join("steps").join(format!("{:02}-{}", i + 1, adb_ui::slug(&name)));
        std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
        let got = capture_state(&adb, serial, spec, &dir)?;
        presses += spec.presses();
        pictures += usize::from(got.screen_file.is_some());
        steps.push(json!({
            "step": i + 1,
            "name": name,
            "keys": step.keys,
            "focused_title": got.focused_title(),
            "fingerprint": got.fingerprint,
            "count": got.nodes.len(),
            "texts": adb_ui::visible_texts(&got.nodes).into_iter().take(40).collect::<Vec<_>>(),
            "files": got.files_json(&outdir),
        }));
    }
    let listing = json!({"version": 1, "serial": serial, "steps": steps, "presses": presses});
    let file = outdir.join("walkthrough.json");
    write_json(&file, &listing)?;
    let e = dev_ev(
        ctx,
        "adb_ui_walkthrough_record",
        serial,
        json!({"serial": serial, "steps": p.steps.len(), "max_nodes": max_nodes, "output_directory": outdir.to_string_lossy()}),
    )
    .result(json!({"file": file, "steps": listing["steps"], "presses": presses}))
    .summary(format!("Walked {} step(s), {presses} key(s) pressed; {pictures} picture(s).", p.steps.len()))
    .location(Location::new("path", file.to_string_lossy()))
    .limitation(Limitation::new("device_changed", "This pressed keys on the device; the app on screen reacted to them."))
    .coverage(Coverage::complete(p.steps.len() as u64))
    .build()?;
    made.keep();
    done(e)
}

/// Why a map stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MapStop {
    /// Every key on every kept screen was pressed.
    Exhausted,
    MaxStates,
    MaxPresses,
    TimeBudget,
    Cancelled,
}

fn adb_app_map(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: MapParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    if !(1..=adb_ui::MAX_MAP_STATES).contains(&p.max_states) {
        return Err(ToolError::invalid(format!("max_states must be between 1 and {}", adb_ui::MAX_MAP_STATES)));
    }
    if !(1..=5000).contains(&p.max_presses) {
        return Err(ToolError::invalid("max_presses must be between 1 and 5000"));
    }
    if !(60..=7200).contains(&p.time_budget_s) {
        return Err(ToolError::invalid("time_budget_s must be between 60 and 7200"));
    }
    if !(1..=adb_ui::MAX_MAP_FOCUS).contains(&p.max_focus_per_screen) {
        return Err(ToolError::invalid(format!("max_focus_per_screen must be between 1 and {}", adb_ui::MAX_MAP_FOCUS)));
    }
    check_at_most(p.launch_wait_ms, 60_000, "launch_wait_ms")?;
    check_at_most(p.gap_ms, 10_000, "gap_ms")?;
    check_at_most(p.wait_ms, 60_000, "wait_ms")?;
    check_at_most(p.dump_lead_ms, 5_000, "dump_lead_ms")?;
    let max_nodes = check_nodes(p.max_nodes)?;
    // The exploration keys: one key each, no repeats.
    let parsed = adb_ui::parse_keys(&p.keys)?;
    if parsed.len() > 20 || parsed.iter().any(|k| k.times != 1) {
        return Err(ToolError::invalid("keys must be 1 to 20 single key names, one per entry (no repeats like `x3`)"));
    }
    let explore: Vec<String> = parsed.into_iter().map(|k| k.key).collect();
    let outdir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&outdir);
    let states_dir = outdir.join("states");
    std::fs::create_dir(&states_dir).map_err(|e| ToolError::io(states_dir.display(), e))?;
    let adb = adb_for_wait(ctx, 120_000)?;
    ensure_online(&adb, serial)?;
    // The entry that starts the app, found once; every capture then relaunches it the same way.
    let (category, _) = launch_app(&adb, serial, package, p.category)?;
    let pre = vec![
        format!("am force-stop {package}"),
        format!("{} >/dev/null 2>&1", launch_command(package, category)),
        format!("sleep {}", adb_ui::seconds(p.launch_wait_ms)),
    ];
    let density = density_of(&adb, serial);
    let size = shell_text(&adb, serial, "wm size", "wm size").ok().and_then(|t| adb_ui::parse_wm_size(&t));
    let started = std::time::Instant::now();
    let budget = Duration::from_secs(u64::from(p.time_budget_s));
    let mut map = adb_ui::ScreenMap::new();
    // Counters the capture closure and the loop both read.
    let presses = std::cell::Cell::new(0u64);
    let captures = std::cell::Cell::new(0u64);
    let mut stop = MapStop::Exhausted;
    // One capture: launch afresh, press `path`, read the screen into a temporary folder, then
    // keep it under the state's id (new), drop it (seen before, skipped, or not the app at all).
    let capture = |map: &mut adb_ui::ScreenMap, from: Option<&str>, key: Option<&str>, path: &[String]| -> Result<Arrived, ToolError> {
        let keys = if path.is_empty() { Vec::new() } else { adb_ui::parse_keys(path)? };
        let spec = CaptureSpec { pre: pre.clone(), key_tokens: path, keys, gap_ms: p.gap_ms, wait_ms: p.wait_ms, dump_lead_ms: p.dump_lead_ms, max_nodes };
        presses.set(presses.get() + spec.presses());
        captures.set(captures.get() + 1);
        let tmp = states_dir.join(format!(".capture-{}", captures.get()));
        std::fs::create_dir(&tmp).map_err(|e| ToolError::io(tmp.display(), e))?;
        let got = match capture_state(&adb, serial, &spec, &tmp) {
            Ok(got) => got,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        // Another package on screen (the launcher after BACK on the home screen, a system
        // dialog): the key left the app. Nothing of it is a screen of the app.
        if let Some(other) = adb_ui::foreground_package(&got.nodes).filter(|f| f != package) {
            let _ = std::fs::remove_dir_all(&tmp);
            let (Some(from), Some(key)) = (from, key) else {
                return Err(ToolError::new(ErrorCode::NotFound, format!("After its launch the screen showed {other}, not {package}."))
                    .with_remediation("Give the app longer to start (launch_wait_ms), or close what is in front of it on the device."));
            };
            map.leave(from, key, &other);
            return Ok(Arrived::Left);
        }
        let skipped = adb_ui::matches_skip(&got.nodes, &p.skip_texts);
        let name = adb_ui::state_name(&got.nodes);
        let focus = got.focused_title();
        let (id, new) = map.arrive(from, key, &got.fingerprint, |id| adb_ui::MapState {
            id,
            name,
            fingerprint: String::new(),
            path: path.to_vec(),
            parent: from.map(str::to_string),
            key: key.map(str::to_string),
            skipped: skipped.clone(),
            focused_title: if skipped.is_some() { None } else { got.focused_title() },
            count: if skipped.is_some() { 0 } else { got.nodes.len() },
            files: None,
            focuses: Vec::new(),
        });
        if skipped.is_some() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok(if new { Arrived::New(id) } else { Arrived::Seen { id, new_focus: false } });
        }
        let new_focus = focus.as_deref().is_some_and(|t| map.note_focus(&id, t, path, p.max_focus_per_screen));
        if new {
            let dir = states_dir.join(&id);
            std::fs::rename(&tmp, &dir).map_err(|e| ToolError::io(dir.display(), e))?;
            let rel = |p: &Path| {
                p.file_name().map(|n| dir.join(n)).and_then(|q| q.strip_prefix(&outdir).ok().map(|r| r.to_string_lossy().to_string())).unwrap_or_default()
            };
            map.set_files(&id, json!({"ui": rel(&got.ui_file), "screen": got.screen_file.as_deref().map(rel), "keys": rel(&got.keys_file)}));
            return Ok(Arrived::New(id));
        }
        // Seen before: nothing of it is kept, but a new focus position on it is walked from.
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(Arrived::Seen { id, new_focus })
    };
    let start = match capture(&mut map, None, None, &[])? {
        Arrived::New(id) | Arrived::Seen { id, .. } => id,
        Arrived::Left => return Err(ToolError::new(ErrorCode::NotFound, format!("The screen after launching {package} wasn't the app's."))),
    };
    // Breadth first over (screen, path): every key from a screen's first position, and from each
    // further focus position on it the keys that act on the focus or move it on.
    let mut frontier = std::collections::VecDeque::from([(start, Vec::<String>::new(), true)]);
    'outer: while let Some((from, path, all_keys)) = frontier.pop_front() {
        let keys: Vec<&String> = explore.iter().filter(|k| all_keys || adb_ui::FOCUS_KEYS.contains(&k.as_str())).collect();
        for key in keys {
            if ctx.cancel.is_cancelled() {
                stop = MapStop::Cancelled;
                break 'outer;
            }
            if map.len() >= p.max_states {
                stop = MapStop::MaxStates;
                break 'outer;
            }
            if presses.get() >= u64::from(p.max_presses) {
                stop = MapStop::MaxPresses;
                break 'outer;
            }
            if started.elapsed() > budget {
                stop = MapStop::TimeBudget;
                break 'outer;
            }
            let mut next = path.clone();
            next.push(key.clone());
            match capture(&mut map, Some(&from), Some(key), &next)? {
                Arrived::New(id) => frontier.push_back((id, next, true)),
                Arrived::Seen { id, new_focus: true } => frontier.push_back((id, next, false)),
                Arrived::Seen { .. } | Arrived::Left => {}
            }
        }
    }
    let kept = map.states().iter().filter(|s| s.skipped.is_none()).count();
    let skipped = map.len() - kept;
    let meta = json!({
        "serial": serial,
        "package": package,
        "launch_category": category,
        "density_dpi": density,
        "size_px": size.map(|(w, h)| json!({"width": w, "height": h})),
        "keys": explore,
        "skip_texts": p.skip_texts,
        "timing": {"launch_wait_ms": p.launch_wait_ms, "gap_ms": p.gap_ms, "wait_ms": p.wait_ms, "dump_lead_ms": p.dump_lead_ms},
        "limits": {"max_states": p.max_states, "max_presses": p.max_presses, "time_budget_s": p.time_budget_s, "stopped_by": stop},
        "presses": presses.get(),
        "captures": captures.get(),
        "seconds": started.elapsed().as_secs(),
    });
    let screens = map.to_json(meta);
    let file = outdir.join("screens.json");
    write_json(&file, &screens)?;
    let why = match stop {
        MapStop::Exhausted => "every key on every screen was pressed".to_string(),
        MapStop::MaxStates => format!("stopped at max_states ({})", p.max_states),
        MapStop::MaxPresses => format!("stopped at max_presses ({})", p.max_presses),
        MapStop::TimeBudget => format!("stopped at time_budget_s ({})", p.time_budget_s),
        MapStop::Cancelled => "stopped because the call was cancelled".to_string(),
    };
    let mut e = dev_ev(
        ctx,
        "adb_app_map",
        serial,
        json!({"serial": serial, "package": package, "category": p.category, "max_states": p.max_states, "max_presses": p.max_presses, "time_budget_s": p.time_budget_s, "keys": p.keys, "skip_texts": p.skip_texts, "output_directory": outdir.to_string_lossy()}),
    )
    .result(json!({"file": file, "count": map.len(), "kept": kept, "skipped": skipped, "presses": presses.get(), "captures": captures.get(), "stopped_by": stop, "states": screens["states"], "edges": screens["edges"], "exits": screens["exits"]}))
    .summary(format!(
        "Mapped {kept} screen(s) of {package} ({skipped} left alone, {} key(s) that leave the app) with {} key press(es) in {} s; {why}.",
        map.exits().len(),
        presses.get(),
        started.elapsed().as_secs()
    ))
    .location(Location::new("path", file.to_string_lossy()))
    .limitation(Limitation::new("device_changed", "This started the app many times and pressed keys on the device."))
    .coverage(Coverage::complete(map.len() as u64));
    if stop != MapStop::Exhausted {
        e = e.limitation(Limitation::new(
            "bounded",
            format!("The map is partial: {why}. Raise the limit, or map from a deeper screen with adb_ui_walkthrough_record."),
        ));
    }
    let e = e.build()?;
    made.keep();
    done(e)
}

/// The crash-log lines grouped by the crashing process, keeping the groups that name `package`.
fn crashes_of(lines: &[adb::LogLine], package: &str) -> Vec<Value> {
    let mut groups: Vec<(Option<u32>, Vec<&adb::LogLine>)> = Vec::new();
    for l in lines {
        match groups.last_mut() {
            Some((pid, group)) if *pid == l.pid && group.len() < 400 => group.push(l),
            _ => groups.push((l.pid, vec![l])),
        }
    }
    groups
        .into_iter()
        .filter(|(_, g)| g.iter().any(|l| l.message.contains(package)))
        .take(50)
        .map(|(pid, g)| {
            let first =
                g.iter().map(|l| l.message.as_str()).find(|m| m.contains("FATAL") || m.contains("Fatal") || m.contains("signal")).unwrap_or(&g[0].message);
            json!({"pid": pid, "timestamp": g[0].timestamp, "first_line": first, "lines": g.iter().map(|l| &l.raw).collect::<Vec<_>>()})
        })
        .collect()
}

fn adb_crash_log_read(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: CrashLogParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let package = adb::validate_package(&p.package)?;
    if !(1..=10000).contains(&p.count) {
        return Err(ToolError::invalid("count must be between 1 and 10000"));
    }
    let adb = adb_for(ctx)?;
    ensure_online(&adb, serial)?;
    let args: Vec<String> =
        ["-s", serial, "logcat", "-d", "-b", "crash", "-v", "threadtime", "-t", &p.count.to_string()].iter().map(|s| s.to_string()).collect();
    let out = adb.run(&args)?;
    let text = usable(out, "logcat")?;
    let lines = adb::parse_logcat(&text);
    let crashes = crashes_of(&lines, package);
    let crashed = !crashes.is_empty();
    let e = dev_ev(ctx, "adb_crash_log_read", serial, json!({"serial": serial, "package": package, "count": p.count}))
        .result(json!({"package": package, "crashed": crashed, "crashes": crashes, "lines_read": lines.len()}))
        .summary(if crashed {
            format!("{package} crashed: {} crash(es) in the log.", crashes.len())
        } else {
            format!("No crash of {package} in the last {} crash-log line(s).", lines.len())
        })
        .coverage(Coverage::complete(lines.len() as u64))
        .build()?;
    done(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::call_err;
    use crate::{CallOptions, Caller, Engine, Session, Settings};

    /// An engine whose adb can never be located: an empty managed store and no override. In Deep
    /// mode, since Clean room refuses most adb tools before adb is looked for.
    fn engine_without_adb() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings { deep_mode: true, ..Default::default() };
        settings.engines.store_dir = Some(dir.path().to_path_buf());
        settings.engines.tools = Default::default();
        (Engine::new(Session::new(settings)), dir)
    }

    fn adb_locatable(e: &Engine) -> bool {
        let s = e.session();
        craft_anything_engines::locate("adb", &s.settings.engines.tools, &s.settings.engine_store_dir()).is_some()
    }

    /// An engine whose adb is a stand-in file that nothing starts, for what a tool refuses before
    /// it runs adb. The engine looks for a tool's program before the tool reads its input, so
    /// without a stand-in a bad-input test passes only on a machine that has adb (docs/pitfalls.md).
    fn engine_with_stand_in_adb() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        std::fs::write(&adb, b"").unwrap();
        let mut session = crate::testing::session();
        session.settings.engines.tools.adb = Some(adb);
        (Engine::new(session), dir)
    }

    /// An engine that finds no adb on any machine: none configured, and this computer isn't searched.
    fn engine_that_finds_no_adb() -> Engine {
        Engine::new(crate::testing::session())
    }

    /// The error of a call that must fail, made with consent given: the device-changing tools are
    /// consent-gated, and what they refuse with consent is what they refuse first.
    fn refused(e: &Engine, id: &str, params: Value) -> ToolError {
        match e.call(id, params, CallOptions { consent: crate::Consent::Granted, ..CallOptions::new(Caller::Mcp) }) {
            Ok(v) => panic!("{id} should fail, got {v:?}"),
            Err(err) => err,
        }
    }

    #[test]
    fn missing_adb_is_provider_unavailable() {
        let (e, _d) = engine_without_adb();
        // Only meaningful when adb is not on this machine's PATH either.
        if adb_locatable(&e) {
            return;
        }
        for id in ["adb_devices_find", "adb_device_show", "adb_log_read", "adb_file_download"] {
            let err = call_err(&e, id, json!({"serial": "emulator-5554", "device_path": "/sdcard", "output_directory": "/tmp/x", "count": 10}));
            assert_eq!(err.code, ErrorCode::EngineMissing, "{id} gave {:?}", err.code);
            assert!(err.remediation.is_some());
        }
    }

    /// The screen tools (R2.12) refuse bad input before adb is run, run in Clean room, and
    /// without adb say so, on a machine with adb and on one without.
    #[test]
    fn screen_tools_check_their_input_first_and_run_in_clean_room() {
        let (e, _d) = engine_with_stand_in_adb();
        for id in ["adb_keys_press", "adb_text_type", "adb_ui_read", "adb_screen_state_capture", "adb_crash_log_read", "adb_app_launch", "adb_device_show"] {
            assert!(crate::clean_room::allows(id), "{id} watches from outside, so Clean room runs it");
        }
        let bad = [
            ("adb_keys_press", json!({"serial": "emulator-5554", "keys": ["POWER"]})),
            ("adb_keys_press", json!({"serial": "emulator-5554", "keys": ["DPAD_UP; reboot"]})),
            ("adb_keys_press", json!({"serial": "emulator-5554", "keys": ["OK"], "gap_ms": 60000})),
            ("adb_text_type", json!({"serial": "emulator-5554", "text": "100%"})),
            ("adb_ui_read", json!({"serial": "emulator-5554", "max_nodes": 0})),
            ("adb_screen_state_capture", json!({"serial": "emulator-5554", "keys": ["OK x99"], "output_directory": "/tmp/x"})),
            ("adb_crash_log_read", json!({"serial": "emulator-5554", "package": "not a package", "count": 10})),
            ("adb_app_launch", json!({"serial": "emulator-5554", "package": "com.example.app", "category": "sideways"})),
        ];
        for (id, params) in bad {
            let err = refused(&e, id, params);
            assert_eq!(err.code, ErrorCode::BadInput, "{id} gave {:?}: {}", err.code, err.message);
        }
        let e = engine_that_finds_no_adb();
        for id in ["adb_keys_press", "adb_text_type", "adb_ui_read", "adb_crash_log_read"] {
            let err = refused(&e, id, json!({"serial": "emulator-5554", "keys": ["OK"], "text": "hi", "package": "com.example.app"}));
            assert_eq!(err.code, ErrorCode::EngineMissing, "{id} gave {:?}", err.code);
        }
    }

    /// The walkthrough and the map (R2.12) check every step and limit before adb is run, run in
    /// Clean room, leave a failed call's folder free, and without adb say so, on a machine with
    /// adb and on one without.
    #[test]
    fn walkthrough_and_map_check_their_input_first() {
        let (e, _d) = engine_with_stand_in_adb();
        for id in ["adb_ui_walkthrough_record", "adb_app_map"] {
            assert!(crate::clean_room::allows(id), "{id} watches from outside, so Clean room runs it");
            assert!(e.registry().get(id).unwrap().effects.needs_consent(), "{id} changes the device");
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out").to_string_lossy().to_string();
        let bad = [
            ("adb_ui_walkthrough_record", json!({"serial": "emulator-5554", "steps": [], "output_directory": out})),
            ("adb_ui_walkthrough_record", json!({"serial": "emulator-5554", "steps": [{"keys": ["POWER"]}], "output_directory": out})),
            ("adb_ui_walkthrough_record", json!({"serial": "emulator-5554", "steps": [{"keys": ["OK"], "wait_ms": 99999}], "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "max_states": 0, "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "max_states": 61, "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "keys": ["DPAD_DOWN x3"], "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "time_budget_s": 5, "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "max_focus_per_screen": 0, "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "com.example.tv", "max_focus_per_screen": 31, "output_directory": out})),
            ("adb_app_map", json!({"serial": "emulator-5554", "package": "not a package", "output_directory": out})),
        ];
        for (id, params) in bad {
            let err = refused(&e, id, params);
            assert_eq!(err.code, ErrorCode::BadInput, "{id} gave {:?}: {}", err.code, err.message);
            assert!(!dir.path().join("out").exists(), "{id}: a refused call made no folder");
        }
        let e = engine_that_finds_no_adb();
        for id in ["adb_ui_walkthrough_record", "adb_app_map"] {
            let err = refused(&e, id, json!({"serial": "emulator-5554", "package": "com.example.tv", "steps": [{"keys": ["OK"]}], "output_directory": out}));
            assert_eq!(err.code, ErrorCode::EngineMissing, "{id} gave {:?}", err.code);
            assert!(!dir.path().join("out").exists(), "{id}: no adb, no folder left behind");
        }
    }

    #[test]
    fn a_launch_command_names_the_entry() {
        assert_eq!(launch_command("com.example.tv", LaunchCategory::Leanback), "monkey -p com.example.tv -c android.intent.category.LEANBACK_LAUNCHER 1");
        assert_eq!(launch_command("com.example.tv", LaunchCategory::Launcher), "monkey -p com.example.tv -c android.intent.category.LAUNCHER 1");
    }

    #[test]
    fn commands_join_with_a_background_dump_and_crashes_group_by_process() {
        let cmds = [
            "input keyevent KEYCODE_DPAD_UP".to_string(),
            "uiautomator dump '/sdcard/x' >/dev/null 2>&1 &".to_string(),
            "sleep 1.000".to_string(),
            "wait".to_string(),
        ];
        assert_eq!(join_commands(&cmds), "input keyevent KEYCODE_DPAD_UP; uiautomator dump '/sdcard/x' >/dev/null 2>&1 & sleep 1.000; wait");
        let (dump, read) = dump_commands("/sdcard/.craft-ui-1.xml");
        assert_eq!(dump, "uiautomator dump '/sdcard/.craft-ui-1.xml' >/dev/null 2>&1");
        assert_eq!(read, "cat '/sdcard/.craft-ui-1.xml'; rm -f '/sdcard/.craft-ui-1.xml'");
        assert!(dump_path("abc").starts_with("/sdcard/.craft-ui-"));
        let log = "11-03 10:00:00.000  1234  1234 E AndroidRuntime: FATAL EXCEPTION: main\n\
                   11-03 10:00:00.001  1234  1234 E AndroidRuntime: Process: com.example.tv, PID: 1234\n\
                   11-03 10:00:00.002  1234  1234 E AndroidRuntime: java.lang.NullPointerException\n\
                   11-03 10:01:00.000  2222  2222 E AndroidRuntime: FATAL EXCEPTION: main\n\
                   11-03 10:01:00.001  2222  2222 E AndroidRuntime: Process: com.other.app, PID: 2222\n";
        let lines = adb::parse_logcat(log);
        let crashes = crashes_of(&lines, "com.example.tv");
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0]["pid"], 1234);
        assert_eq!(crashes[0]["lines"].as_array().unwrap().len(), 3);
        assert!(crashes[0]["first_line"].as_str().unwrap().contains("FATAL"));
        assert!(crashes_of(&lines, "com.nobody").is_empty());
    }

    /// #234: a call that fails before writing anything leaves no folder, so the same call can be
    /// tried again; a folder with something in it, or a kept one, stays.
    #[test]
    fn a_failed_call_leaves_its_output_folder_free() {
        let (e, d) = engine_without_adb();
        if !adb_locatable(&e) {
            for id in ["adb_screenshot_take", "adb_bugreport_save", "adb_file_download", "adb_app_download"] {
                let out = d.path().join(id);
                let args = json!({"serial": "emulator-5554", "package": "com.example.app", "device_path": "/sdcard/x", "output_directory": out});
                assert_eq!(call_err(&e, id, args.clone()).code, ErrorCode::EngineMissing, "{id}");
                assert!(!out.exists(), "{id} left {out:?} behind");
                assert_eq!(call_err(&e, id, args).code, ErrorCode::EngineMissing, "{id}: the retry was blocked");
            }
        }
        let (empty, written, kept) = (d.path().join("empty"), d.path().join("written"), d.path().join("kept"));
        for dir in [&empty, &written, &kept] {
            std::fs::create_dir(dir).unwrap();
        }
        std::fs::write(written.join("part"), b"x").unwrap();
        drop(UnlessWritten::new(&empty));
        drop(UnlessWritten::new(&written));
        UnlessWritten::new(&kept).keep();
        assert!(!empty.exists() && written.exists() && kept.exists());
    }

    #[test]
    fn inspect_client_never_needs_adb() {
        let (e, _d) = engine_without_adb();
        // engines_adb_check is Requires::Nothing: it answers even with no adb installed.
        let out = e.call("engines_adb_check", json!({}), CallOptions::new(Caller::Cli)).unwrap();
        let v = out.to_json();
        assert_eq!(v["result"]["available"], adb_locatable(&e));
    }

    #[test]
    fn bad_serial_is_rejected() {
        let (e, _d) = engine_without_adb();
        if adb_locatable(&e) {
            return;
        }
        // A hostile serial must be rejected regardless; when adb is missing the provider error
        // comes first, so test validation directly.
        assert!(adb::validate_serial("a; rm -rf /").is_err());
    }

    #[test]
    fn device_mutating_tools_need_consent() {
        // With adb present but no consent, a device-changing tool is refused before running.
        let (e, _d) = engine_without_adb();
        if !adb_locatable(&e) {
            // No adb → provider error precedes consent; the effect flags still mark it destructive.
            let spec = e.registry().get("adb_app_install").unwrap();
            assert!(spec.effects.needs_consent());
            assert!(e.registry().get("adb_activity_open").unwrap().effects.needs_consent());
            return;
        }
        let err = call_err(&e, "adb_app_install", json!({"serial": "emulator-5554", "apk_path": "/tmp/x.apk"}));
        assert!(matches!(err.code, ErrorCode::ConsentRequired | ErrorCode::NotFound | ErrorCode::EngineMissing));
    }

    /// #235: Android's reply for a missing package names it, and was read as the package.
    #[test]
    fn a_missing_package_is_not_found() {
        assert!(!package_found("Unable to find package: com.example.missing\n", "com.example.missing"));
        assert!(!package_found("Packages:\n  Package [com.example.other] (1a2b):\n", "com.example"));
        assert!(package_found("Packages:\n  Package [com.example.app] (1a2b3c):\n    versionName=1.0\n", "com.example.app"));
    }

    #[test]
    fn parsers_and_fragments() {
        let fields = grep_fields("  versionName=1.2.3 flags=0x1\n  versionCode=42 minSdk=21\n", &["versionName=", "versionCode=", "minSdk="]);
        assert_eq!(fields.get("versionName").and_then(Value::as_str), Some("1.2.3"));
        assert_eq!(fields.get("versionCode").and_then(Value::as_str), Some("42"));
        assert_eq!(fields.get("minSdk").and_then(Value::as_str), Some("21"));

        let ls = parse_ls("total 8\n-rw-rw-rw- 1 root root 1024 2024-01-01 12:00 file.txt\ndrwxr-xr-x 2 root root 4096 2024-01-01 12:00 dir\n");
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].size, Some(1024));
        assert_eq!(ls[0].name, "file.txt");

        assert_eq!(extra_fragment("k", ExtraType::Boolean, "true").unwrap(), "--ez k true");
        assert_eq!(extra_fragment("k", ExtraType::Int, "5").unwrap(), "--ei k 5");
        assert!(extra_fragment("k", ExtraType::Int, "notnum").is_err());
        assert_eq!(extra_fragment("k", ExtraType::String, "a b").unwrap(), "--es k 'a b'");
    }

    #[test]
    fn clip_respects_byte_limit_on_multibyte() {
        assert_eq!(clip("aé", 2), ("a".to_string(), true));
        assert_eq!(clip("a€b", 2), ("a".to_string(), true));
        assert_eq!(clip("abc", 2), ("ab".to_string(), true));
        assert_eq!(clip("abc", 3), ("abc".to_string(), false));
    }
}
