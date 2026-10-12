# Port notes — EffectCraft on Android

Upstream: [`storytold/effectcraft`](https://github.com/storytold/effectcraft) @
`0b2d85b81…` (v0.8.0, see the release page for the full pinned sha)
Published APK: `effectcraft-0.8.0-arm64-v8a.apk`

The port is `apps/effectcraft-android/` (`src/lib.rs`, `src/file_picker.rs` 403, `src/modifiers.rs` 92,
`src/prefs_watch.rs` 139) plus **5 upstream files** — 63 added lines in total.

## 1. The shell crate

`effectcraft_ui_egui::Hooks` is a struct of `Option<Box<dyn Fn…>>` fields that derives `Default`; the
desktop binary and the browser shell fill it their own way, and Android is the third.
`effectcraft-host` supplies the wired `Session` — FilmCraft's codecs, the media importer, the
expression engine, JavaScript scripting and the `FileExporter` — and none of it needed a change.

## 2. The dialogs: the app's own asynchronous route, registered first

`Hooks` carries the blocking pickers (`pick_files`, `pick_save`, `pick_open_project`, `pick_folder`,
`pick_save_file`), which cannot work on a host that draws its own dialog — a function that has to
return a path *now* can never show one — and it also carries the seam the macOS build uses:

```rust
pub file_dialog_async: Option<Box<dyn Fn(FileDialog, DialogDone)>>,
```

The shell registers **that** with the in-app picker, and `menus::file_dialog` consults it first for
every command that needs a path — File ▸ Import / Import Multiple / Open / Save / Save As / Save a
Copy As XML / Export ▸ Lottie JSON / Export ▸ Adobe Premiere Pro Project / Collect Files / Relink
Missing Footage / Watch Folder / Run Script / Install Script / Set Proxy / Save Frame As… all open a
working dialog.

The blocking slots stay `None` **on purpose**, and every panel that reaches one already has a written
fallback for a host without a dialog: Settings ▸ paths say "type the path", the Effect Controls'
`Choose…` says so too, and the Render Queue's *Output To* falls back to its inline name template —
which is what After Effects itself does on a machine with no file dialog. Registering them with a stub
that returns "cancelled" would have been *worse*: those panels would silently do nothing.

## 3. `file.exportTimeline` had no dialog route at all — a real bug, not an Android one

Enumerating the dialog call sites (rather than trusting the seam) turned up a command that reached no
picker by any route: File ▸ Export ▸ Adobe Premiere Pro Project… needs a `path`, asked nobody for it,
and failed with *"missing `path`"*. It now asks with a Save dialog, defaulting to the Final Cut Pro
XML that Premiere Pro imports and that `export_timeline` itself infers from an `.xml` name (Premiere's
own `.prproj` is not written). This fixes the desktop build too.

## 4. `File ▸ Exit`: the app gained a flag for a host with no window to close

`ViewportCommand::Close` ends nothing on Android — `egui-winit` turns it into a plain
`ViewportEvent::Close` and winit's Android window API is stubbed — so an app that answers Exit with a
close request would sit there looking frozen, die on the next input event, and refuse to reopen until
it was cleared from Recents. EffectCraft's exit path did exactly that, and it had no way for a host to
know a quit had been agreed.

So the app gains two fields, and three small edits keep them honest:

```rust
pub quit_requested: bool,        // the user asked to quit; the *host* ends the process
pub(crate) quit_pending: bool,   // a quit waiting on an untitled project's Save As
```

- A quit of a **clean** project sets `quit_requested`; a **modified** one never does — the unsaved
  prompt asks first, and answering *that* is what sets the flag (`panels/unsaved.rs`).
- A quit that has to wait for a Save As dialog sets `quit_pending` and lets
  `menus::dialog_answered` finish it once the file is written — the path is chosen between frames, so
  the quit cannot proceed inside the click that asked for it. A **cancelled** dialog clears the
  pending quit with it.
- The shell checks `quit_requested` in **both** `logic` and `ui`, because the menu click that sets it
  happens during `ui` and no repaint is guaranteed afterwards — which is exactly how File ▸ Exit comes
  to do nothing until the user clicks a second time in the unfixed shape.

The desktop ignores both fields and gets a real window close from the command as before.

## 5. Fonts

`crates/text/src/fonts.rs` takes its Android branch now: `/system/fonts`, `/product/fonts`,
`/system_ext/fonts`, `/vendor/fonts`. Without it Android fell through to the Linux font tree
(`/usr/share/fonts`, `~/.fonts`), none of which exists on a device, so the app knew only its bundled
families and every document set in the device's own typeface fell back with no error at all. The
directories are read without any permission.

## 6. The rest of the shell

- **Preferences written while the app runs** (`prefs_watch.rs`), change-detected, at most every two
  seconds, atomic temp-file-and-rename — the destination folder is created first, because
  `write_atomic` writes its temporary file *inside* it and a missing folder would be an `ENOENT` on
  every write.
- **Modifier keys** (`modifiers.rs`), tracked from the key events in `raw_input_hook` and re-injected
  the way egui expects, since winit's Android backend never emits `ModifiersChanged`.
- **The in-app picker** (`file_picker.rs`) on EffectCraft's `FileDialog`, floating above the app,
  sized from `ctx.content_rect()`, finger-sized padding, listings and readability probes cached per
  visit, locked roots left tappable so they name the permission and the Settings path, "Show all
  files" with a count of what the filter hides, and a Save answer of `dir.join(name)`.
- **`config_changes`** in the APK metadata, so rotation, the keyboard, density, font scale and
  split-screen resizing do not recreate the Activity.
- **A file logger** in the app's own data directory, because `logcat` needs adb.
- **`[lib] name = "effectcraft"`**, `crate-type = ["cdylib"]` — the injected `android.app.lib_name`
  is `effectcraft` and the APK ships `lib/<abi>/libeffectcraft.so`.
- **A recovery folder** and the settings store under `internal_data_path()`.

## 7. The accesskit trap

`crates/ui-egui/Cargo.toml` declared eframe with `accesskit`. `eframe` `compile_error!`s when
`accesskit` meets `android-native-activity`, and Cargo **unions** features across the whole graph — so
this has to be fixed in the one crate that enables `accesskit`, not from the Android shell outside
it. eframe is declared twice: the base dependency without `accesskit`, and
`[target.'cfg(not(target_os = "android"))'.dependencies]` with it. Verified in **both**
directions — the host check and the Android check.

## Upstream files this port changes (5)

| File | Change | Why |
|---|---|---|
| `crates/ui-egui/Cargo.toml` | eframe declared per target (`accesskit` off for Android) | the `compile_error!` pair; the union of features means it cannot be fixed from outside (§7) |
| `crates/ui-egui/src/lib.rs` | `quit_requested` / `quit_pending` on the app | a host with no window to close needs to be told a quit was agreed (§4) |
| `crates/ui-egui/src/menus.rs` | the quit path sets the flag; `file.exportTimeline` asks for a path | §4, and a command that had no dialog route at all (§3) |
| `crates/ui-egui/src/panels/unsaved.rs` | the unsaved-changes prompt's answer sets the quit | a modified project is never killed silently (§4) |
| `crates/text/src/fonts.rs` | Android font directories | no candidate matched on Android, so the app had no system fonts (§5) |

## What is deliberately *not* in the patch

`Cargo.lock` is excluded on purpose: it moves only because the port crate is a new workspace member,
and `cargo` regenerates it on the first build.

## What Android cannot do yet

- **No audio.** `cpal` has no backend for this target here, so `Hooks::audio_device` and
  `audio_devices` stay `None`: preview and scrub are silent. The codecs and the render pipeline are
  unaffected.
- **No system clipboard.** `Edit ▸ Paste` falls back to egui's own text handling — text typed into an
  egui field and anything copied inside the app still work — but a string copied in another app is not
  offered.
- **No system browser or file manager**, so the Help ▸ links that would open the website or Discord do
  nothing.
- **No stylus pressure or tilt.** A pen degrades to ordinary touch.
- **No system title bar.** `integrated_titlebar` is off, as on Windows and Linux, so the app keeps its
  own menu bar and header.

## Not verified here

Everything above is source- and artifact-level: the APK installs, is signed, declares `arm64-v8a`,
exports `android_main` and `ANativeActivity_onCreate`, and its `lib_name`, `configChanges` and theme
verify. The emulator run (picker, exit, preferences) is recorded in the [README](README.md)'s
verification table; the picker's touch targets, the quit path's feel and anything about a specific
device's GPU are only real on a tablet.
