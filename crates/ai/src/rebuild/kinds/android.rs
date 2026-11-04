//! An app for a phone or TV, studied on a device and built with Kotlin and Jetpack Compose. The
//! build is judged by its Gradle build and, with a device, by replaying the mapped screens on it.

use super::*;

pub struct AndroidAppKind;

impl AndroidAppKind {
    pub const ID: &'static str = "android-app";
    /// The stack a device target is built with unless the request names another.
    pub const STACK: &'static str = "Kotlin + Jetpack Compose (Android)";
}

/// What the builder is told about an app studied on a device: what capture/ holds, how to read
/// positions, what the device check claims, and what not to do.
pub const DEVICE_BRIEF: &str = "Working from the map: capture/map/screens.json is the list of screens to rebuild, each reached from the app's launch by the keys in its `path`, with map/states/<id>/ui.json (every control: role, text, identifier, position and size in px and in dp, focused, focusable, selected, scrollable) and screen.png. Rebuild each kept screen as its own Composable with the same texts, the same controls in the same order, and a focus that lands where the original's did; take sizes and positions from the dp values. A screen marked skipped has no files and isn't rebuilt. The app is for a TV when the device is one: every screen is driven by the D-pad, OK, BACK and MENU, never by touch, and the MAIN activity is in the LEANBACK_LAUNCHER category. After each build Craft Anything installs the debug APK on the device, replays each screen's keys and claims, per screen: the same control has the focus, every control of the original is there by its exact text, paired controls sit within a dp or two, and nothing crashed; rebuild_check shows you the result while you work. There is no --screenshot option to add. To look at your own build on the device, use the craft-anything adb tools (adb_app_install, adb_app_launch with category leanback, adb_keys_press, adb_ui_read, adb_screen_state_capture, adb_crash_log_read) on the build device from Settings; never run adb from a shell, and never drive or read the original app: the map is all of it you get.";

impl TargetKind for AndroidAppKind {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn words(&self) -> KindWords {
        KindWords {
            button: "Install on the device",
            icon: "app-window",
            ready: "Your app is on the device",
            change_hint: "Ask for a change, e.g. \"make the guide rows taller\" or \"add a favourites screen\"",
        }
    }

    /// An app on a device is this kind whatever the request says ("recreate this IPTV player"
    /// never means a game); a request for an Android or TV app with no target is too.
    fn detect(&self, cx: &DetectCx) -> Option<u32> {
        if matches!(cx.target, TargetRef::Device { .. }) {
            Some(300)
        } else if cx.says(&["android app", "android tv", " apk", "tv app", "app for my tv", "app for android"]) {
            Some(80)
        } else {
            None
        }
    }

    fn default_stack(&self, _cx: &DetectCx) -> &'static str {
        Self::STACK
    }

    fn goal(&self) -> &'static str {
        "an Android app that behaves the same on the same device: the same screens, controls, focus order, texts and layout, judged on the device"
    }

    fn brief_sections(&self, _cx: &BriefCx) -> Vec<String> {
        vec![DEVICE_BRIEF.to_string()]
    }

    /// The device check pictures the build on the device; the builder adds no screenshot option.
    fn self_screenshots(&self) -> bool {
        false
    }

    fn rebuild_words(&self) -> &'static str {
        "build it again and install it"
    }

    fn checks(&self, _ws: &Workspace) -> Vec<Box<dyn Check>> {
        vec![Box::new(BuildCheck)]
    }
}

/// Where the JDK and the Android SDK are on this computer, as the engines table finds them
/// (Settings → Engines overrides, PATH, the usual folders): what a Gradle build needs.
pub fn android_homes(settings: &craft_anything_engine::Settings) -> Option<super::super::check::AndroidHomes> {
    let store = settings.engine_store_dir();
    let javac = craft_anything_engines::locate("jdk", &settings.engines.tools, &store)?;
    let sdkmanager = craft_anything_engines::locate("androidsdk", &settings.engines.tools, &store)?;
    super::super::check::AndroidHomes::from_tools(&javac, &sdkmanager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_target_is_an_android_app_whatever_the_words_and_builds_with_kotlin() {
        let kinds = KindRegistry::built_in();
        let device = TargetRef::Device { serial: "192.168.1.20:5555".into(), package: "com.example.tv".into() };
        let (k, stack) = kinds.choose("Recreate this IPTV player 1:1, screen for screen", &device, false).unwrap();
        assert_eq!((k.id(), stack), (AndroidAppKind::ID, AndroidAppKind::STACK), "\"player\" doesn't make it a game");
        assert_eq!(kinds.choose("an android tv app that plays streams", &TargetRef::None, false).unwrap().0.id(), AndroidAppKind::ID);
        assert_eq!(kinds.choose("a racing game", &TargetRef::None, false).unwrap().0.id(), GameKind::ID);
        assert_eq!(kinds.choose("a notes tool", &TargetRef::None, false).unwrap().0.id(), AppKind::ID, "app is still the fallback");
        assert_eq!(
            kinds.choose("the same app, in Flutter", &device, false).unwrap().1,
            AndroidAppKind::STACK,
            "an unknown stack name keeps the default; the brief passes the request on"
        );
        assert!(!AndroidAppKind.self_screenshots());
        assert!(!DEVICE_BRIEF.contains("--screenshot option to add") || DEVICE_BRIEF.contains("There is no --screenshot"));
    }
}
