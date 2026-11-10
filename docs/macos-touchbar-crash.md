# macOS Touch Bar shutdown crash

## Report and diagnosis

A native Apple Silicon build on a Touch Bar MacBook Pro (MacBookPro17,1,
macOS 27.0) aborted when Quit was selected during installation verification.
The main-thread exception passes through Foundation observer removal,
`_NSTouchBarFinderObservation::invalidate`, AppKit's display-cycle flush and
eframe's `run_and_return`. This is an Objective-C exception, not a Rust panic
or a Codex provider error. The raw report stays private: it contains machine
identifiers and local paths.

The locked `accesskit_macos` 0.26.3 implementation unconditionally restores the
view's previous runtime class when its subclassing adapter drops. AppKit can
have installed a KVO subclass above AccessKit's subclass. Restoring the older
class discards that observation; AppKit subsequently tries to remove it and
raises an exception. This matches the reported stack and the upstream fix in
[AccessKit #835](https://github.com/AccessKit/accesskit/pull/835), released in
`accesskit_macos` 0.27.2. Stack agreement is strong evidence, not a claim that
the original app's internal view state was inspected under a debugger.

## Change and scope

The 0.36 GUI stack requires the 0.26 macOS adapter line, so a lockfile-only
0.27.2 update cannot satisfy its dependency constraints. Move the coordinated
eframe, egui, egui_extras and egui_kittest dependencies to published 0.37
releases, which resolve the fixed 0.27.2 adapter. Keep the workspace's existing
Rust minimum; do not change the graphics backend or add default features.

The source migration is mechanical: `WidgetType` becomes `Role`, including
`Checkbox` → `CheckBox`, `SelectableLabel` → `Button` and `Other` → `Unknown`
(the same native roles the previous version produced). `Id::unique` replaces
the deprecated `Id::new` alias; `scope_id` replaces `Ui::id`. These renames do
not intentionally alter control labels, IDs, styles or product behavior.

The newer test harness also makes missing glyphs an error. Component tests now
install normal fonts and image loaders before their first frame, rather than
letting unloaded SVG icons enter the generic error-image path. The locale
selection test explicitly permits placeholder glyphs in minimal builds without
optional CJK fonts; it asserts translated labels and persistence, not typography.
The other UI tests retain strict glyph checking.
The stricter accessibility checks also exposed unnamed settings selectors;
their existing translated row titles now name the controls without adding visible
text. Locale tests identify the selector by role as well as label, since the
visible row title and accessible control now correctly share that name.
Bundled JetBrains Mono is also a proportional-font fallback for navigation
arrows when optional craft-fonts are absent; a strict-glyph regression renders
those hints in regular, medium and semibold. Modal smoke checks wait for their
accessibility-tree frames after observing the asynchronous state change.

Accessibility remains enabled. No exception suppression, `unsafe` application
code, private AppKit API, forced process exit, or vendored dependency is added.
Normal preference saving, job cancellation and child-process cleanup retain
their existing shutdown path. The broader dependency migration is intentional
and should be reviewed independently of the Codex discovery/sign-in feature.

## Regression and verification

`apps/craft-anything/tests/accessibility_lock.rs` parses the checked-in lockfile
and requires every macOS adapter resolution to include a stable release at
least 0.27.2. Fixtures reject the old 0.26.3 resolution, pre-fix versions,
prereleases, invalid/missing packages and mixed fixed/vulnerable resolutions.
This is a dependency regression guard, **not** an automated native crash test.

Before calling this fixed, build and run the native app on the affected Mac,
exercise both Quit and the red window-close button, check process exit status
and newly created DiagnosticReports, and verify saved UI settings after reopening.
Headless GUI tests, accessibility-tree checks and clippy supplement that test;
they cannot substitute for AppKit's actual display-cycle teardown. Windows,
Linux and VoiceOver require their own live checks before an upstream release.

On the affected M1 Touch Bar Mac, the earlier personal-fork candidate produced normal `closed` log
markers after menu Quit and window close, with no new Craft Anything crash
reports. Reopening retained the selected Codex backend and Advanced mode.
The native accessibility tree and window contents were inspected. These were
idle-app checks: shutdown during an active build and VoiceOver were not tested,
and launchd's numeric exit status was not captured. The existing exit-settings
integration test separately checks pending settings and AI settings preservation.

The upstream candidate was separately verified on 2026-11-09 on the same class
of hardware, using a distinct test bundle and temporary configuration directory.
Menu Quit and the native red window-close control each returned process exit
code 0, produced normal `closed` markers and created no new matching diagnostic
reports. Reopening retained Advanced mode in the temporary settings; no account
was connected for these checks. The installed application was not replaced.
The desktop/GUI suites passed 238 tests (one existing ignored), including the
dependency guard, strict arrow-glyph regression and exit-settings integration.
All-target clippy with warnings denied, fmt, layering, lock, translation and secret
checks passed; the native desktop and companion CLI built with locked dependencies.

## Recovery and upstream review

Keep the prior application bundle and settings before installing. A local
ad-hoc-signed build is not a notarized release. Rollback installs the retained
bundle; reverting the dependency migration also restores the known crash risk.
Do not remove user projects, authentication stores or settings during recovery.

An upstream submission should include the sanitized stack, the exact dependency
versions, this rationale for avoiding early-exit/accessibility workarounds,
the lockfile regression guard, and the observed native test results. Do not
attach raw account logs or the private crash report.
