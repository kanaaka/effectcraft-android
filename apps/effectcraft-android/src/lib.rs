//! EffectCraft on Android (arm64-v8a): the real engine and egui shell, running on winit's
//! NativeActivity backend.
//!
//! # What this crate is
//!
//! A shell around the *unmodified* EffectCraft crates — `effectcraft-host` (the wired `Session`:
//! FilmCraft's codecs, the importer, the expression engine, scripting and the `FileExporter`) and
//! `effectcraft-ui-egui` (the docking panels, composition viewer, timeline, effect controls and
//! every dialog). This crate supplies only what `apps/effectcraft` would normally supply: the
//! `Hooks` implementation, the settings store, the recovery folder, the log, the window options and
//! the entry point. The same shape the DeckCraft, VectorCraft, WordCraft, GridCraft, PhotoCraft and
//! PdfCraft ports used.
//!
//! # Why a separate crate, and not a feature on `apps/effectcraft`
//!
//! That crate cannot build for Android as written: it carries `rfd` (native file dialogs), `cpal`
//! (the audio sink), a localhost control server, an X11 event-loop preference, Windows/macOS menu
//! code and `winresource`. `effectcraft_ui_egui::Hooks` is a plain struct of `Option<Box<dyn Fn…>>`
//! fields that derives `Default`, so the fix is not to patch the app but to build a platform that
//! supplies what it can and omits the rest.
//!
//! # Which half of the seam this shell fills
//!
//! EffectCraft's dialogs have two shapes. `pick_files` / `pick_save` / `pick_open_project` /
//! `pick_folder` / `pick_save_file` are the **blocking** ones — the desktop runs `rfd::FileDialog`
//! in line and returns a path immediately. That cannot work here: a picker this shell has to *draw*
//! needs the frame to end and come back, so a function that must return a path now can never show
//! one.
//!
//! The app already has the right seam for that, and it is the one the macOS build uses:
//! `Hooks::file_dialog_async: Option<Box<dyn Fn(FileDialog, DialogDone)>>` — the host is handed the
//! request plus a callback and answers from a later frame. [`android_services`] registers it with
//! the in-app picker ([`file_picker`]), and `menus::file_dialog` consults it **first** for every
//! command that needs a path, so File ▸ Import / Import Multiple / Open / Save / Save As / Save a
//! Copy As XML / Export ▸ Lottie JSON / Export ▸ Adobe Premiere Pro Project / Collect Files /
//! Relink Missing Footage / Watch Folder / Run Script / Install Script / Set Proxy / Save Frame As
//! and the rest all open a working dialog.
//!
//! The blocking slots stay `None` on purpose, and every panel that reaches one has a written
//! fallback for a host without a dialog: Settings ▸ paths say "type the path", the Effect Controls'
//! Choose… says so too, and the Render Queue's Output To falls back to its inline name template
//! (which is what After Effects does on a machine with no file dialog). Registering them with a
//! stub that returns "cancelled" would be *worse*: the panels would silently do nothing.
//!
//! # Where renders go
//!
//! A render's destination is `prefs.export.default_output_folder` joined with the output module's
//! name template, unless the template is absolute (`Session::resolve_output`). On Android the
//! process' working directory is `/`, which nothing can write, so the shell fills that setting in
//! with the app's own external files folder when it is empty (see [`default_output_folder`]). The
//! Render Queue's Output To then resolves to a real, writable path, and the Export dialogs ask for
//! one through the picker.
//!
//! # What this shell deliberately does not register
//!
//! - **No audio.** `cpal` has no Android backend, so `Hooks::audio_device` and `audio_devices` stay
//!   `None`: Preview playback and audio scrubbing are silent, and Settings ▸ Audio lists no
//!   devices. That is the same silence the desktop gives on a machine with no output device.
//! - **No system clipboard.** `arboard` has no Android backend, so `clipboard_text` stays `None`:
//!   Edit ▸ Paste falls back to egui's own text handling — text typed into an egui field and
//!   anything copied inside the app still work — but a string copied in another app is not offered.
//! - **No system browser or file manager.** `Hooks::app_action` stays `None`, so the Help ▸ links
//!   that would open the website or Discord do nothing.
//! - **No stylus pressure or tilt.** A pen degrades to ordinary touch.
//! - **No system title bar.** `integrated_titlebar` is off, as on Windows and Linux, so the app
//!   keeps its own menu bar and header rather than a title bar that nothing draws.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use effectcraft_ui_egui::{DialogDone, EffectcraftApp, FileDialog, Hooks};

mod file_picker;
mod modifiers;
mod prefs_watch;

/// The app's private data directory, captured in `android_main` before any service closure runs:
/// `<internal storage>/Android/data/ai.storyteller.effectcraft/files`.
///
/// `internal_data_path()` needs no runtime permission — it is the app's own sandbox — which is why
/// the settings, the log and the crash-recovery sentinel land here rather than in shared storage.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Messages the shell shows the user when work that happened off the frame failed.
type Notices = Arc<Mutex<Vec<String>>>;

fn app_data_dir() -> Option<&'static Path> {
    DATA_DIR.get().map(PathBuf::as_path)
}

/// Where a render whose output name is relative goes.
///
/// `Session::resolve_output` joins a relative output name onto `prefs.export.default_output_folder`
/// (then the project's folder, then `~/Movies` or the home folder). On Android none of those exist:
/// `HOME` is unset and the working directory is `/`, which nothing can write — so without this every
/// Render Queue render would fail on a permission error. The app's own external files folder is
/// always writable with no permission at all, and it is where a sideloaded build's user can actually
/// find the file.
fn default_output_folder() -> Option<String> {
    if let Some(ext) = file_picker::EXT_DIR.get() {
        let renders = ext.join("Renders");
        let _ = std::fs::create_dir_all(&renders);
        return Some(renders.to_string_lossy().to_string());
    }
    let dir = app_data_dir()?.join("Renders");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.to_string_lossy().to_string())
}

/// A settings store in a directory that always exists on the device.
///
/// `effectcraft_host::config_dir()` resolves `$XDG_CONFIG_HOME/effectcraft`, `~/.config/effectcraft`
/// or `%APPDATA%\EffectCraft`; on Android `HOME` is unset and none of those exist, so without this
/// the session would have **no config store at all** and nothing would ever be written — settings,
/// shortcut presets, the saved workspace layout (which the app saves live, but only when there is a
/// store) and the crash-recovery sentinel all silently lost.
fn config_store() -> Arc<dyn effectcraft_engine::config::ConfigStore> {
    Arc::new(effectcraft_engine::config::DirConfig::new(app_data_dir().unwrap_or(Path::new("."))))
}

/// How often the settings mirror is looked at (see [`prefs_watch`]).
const PREFS_EVERY: Duration = Duration::from_secs(2);

/// The settings as they are written to `prefs.json`.
fn prefs_bytes(app: &EffectcraftApp) -> Vec<u8> {
    app.session.prefs.to_json().into_bytes()
}

/// Start the log, switched on, in the app's own directory.
///
/// The app's own logger writes to `Logs/EffectCraft Log.txt` once it is enabled (Help ▸ Enable
/// Logging) and always keeps the last few hundred warnings in memory for the System Compatibility
/// Report. `logcat` is only reachable over adb and a sideloaded build has none, so the file is
/// switched on here: it is the trace a user can pull with any file manager. Installing the app's
/// logger (rather than a shell-local one) keeps the compatibility report working.
fn init_logging(dir: &Path) {
    effectcraft_engine::logging::install();
    effectcraft_engine::logging::install_panic_hook();
    let path = effectcraft_engine::logging::log_path(Some(dir));
    match effectcraft_engine::logging::set_enabled(true, path.clone()) {
        Ok(()) => log::info!("logging to {}", path.display()),
        Err(e) => eprintln!("effectcraft: cannot log to {}: {e}", path.display()),
    }
}

/// The Android `Hooks`: the asynchronous dialog seam, plus the device-loss report.
///
/// `ctx` is kept so the picker can ask for a frame while it is open — the app only wakes the window
/// about once a second while it waits for an answer, which would make every tap feel dead.
fn android_services(ctx: egui::Context, _notices: Notices, device_lost: Arc<AtomicBool>) -> Hooks {
    Hooks {
        // The whole dialog seam. `menus::file_dialog` consults this *first* for every command that
        // needs a path, so no command can end up asking a blocking hook instead.
        file_dialog_async: Some(Box::new(move |dialog: FileDialog, done: DialogDone| {
            let (kind, title, exts, suggested, ext): (file_picker::Kind, &str, Option<Vec<String>>, Option<String>, String) = match dialog {
                FileDialog::Open { exts, many } => {
                    let (kind, title) = if many {
                        (file_picker::Kind::OpenMany, "Import Files \u{2014} Android")
                    } else {
                        (file_picker::Kind::Open, "Open \u{2014} Android")
                    };
                    (kind, title, (!exts.is_empty()).then_some(exts), None, String::new())
                }
                // A project: `.ecproj` (JSON) or the XML copy `Save a Copy As XML` writes.
                FileDialog::OpenProject => (
                    file_picker::Kind::Open,
                    "Open Project \u{2014} Android",
                    file_picker::extensions(&[&["ecproj", "ecprojx"]]),
                    None,
                    String::new(),
                ),
                FileDialog::Folder => (file_picker::Kind::Folder, "Choose Folder \u{2014} Android", None, None, String::new()),
                FileDialog::Save { default, ext, project } => {
                    let title = if project { "Save Project \u{2014} Android" } else { "Save As \u{2014} Android" };
                    let exts = (!ext.trim().is_empty()).then(|| vec![ext.clone()]);
                    (file_picker::Kind::Save, title, exts, Some(default), ext)
                }
            };
            file_picker::request(kind, title, exts, suggested, &ext, done);
            // The frame the menu click arrived in is over; the picker needs the next one.
            ctx.request_repaint();
        })),
        // The desktop pops an `rfd` message box and closes the window. There is no message box to
        // pop — and a lost device cannot draw one anyway — so the reason goes to the log and the
        // process ends at the end of this frame. The auto-save the app made just before this call is
        // what the next launch offers to recover (the device-loss path keeps the sentinel).
        device_lost: Some(Box::new(move |saved: Option<Result<String, String>>| {
            let saved = match saved {
                Some(Ok(path)) => format!("unsaved changes were auto-saved to {path}; the next launch offers to recover them"),
                Some(Err(e)) => format!("unsaved changes could NOT be saved ({e})"),
                None => "nothing was unsaved".to_string(),
            };
            log::error!("the graphics device was lost: {saved}; closing");
            device_lost.store(true, Ordering::Relaxed);
        })),
        ..Default::default()
    }
}

/// The Android entry point.
///
/// `android-activity` spawns a dedicated thread for this and calls it with the activity handle.
/// `#[unsafe(no_mangle)]` with the Rust ABI is the required shape (edition 2024 requires the
/// `unsafe(..)` wrapper around `no_mangle`), and it is the symbol the injected NativeActivity looks
/// for.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    // Count this window while its loop runs (see `prefs_watch`).
    let _window = prefs_watch::counted_window();
    let data_dir = app.internal_data_path().unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&data_dir);
    let _ = DATA_DIR.set(data_dir.clone());
    init_logging(&data_dir);
    log::info!("EffectCraft {} starting on Android; data dir {}", env!("CARGO_PKG_VERSION"), data_dir.display());
    // The app's external files directory: always readable and writable with no permission, so the
    // picker offers it as a root, Save falls back to it, and renders land in it.
    if let Some(ext) = app.external_data_path() {
        let _ = std::fs::create_dir_all(&ext);
        let _ = file_picker::EXT_DIR.set(ext);
    }
    // Create the USB-visible media folder so it is there for files to be copied into: Android 11+
    // hides `Android/data` from MTP, so this is the one a user can reach with a cable.
    if let Some(media) = file_picker::media_dir() {
        let _ = std::fs::create_dir_all(&media);
    }
    // The system font folders. `DirectorySource::system` knows Android's own now (`/system/fonts`,
    // `/product/fonts`, `/system_ext/fonts`, `/vendor/fonts`); without this scan the Font menus would
    // list the bundled families only, and every document set in the device's own typeface would fall
    // back with no error at all.
    effectcraft_engine::text::fonts::scan_system_in_background();

    let notices: Notices = Notices::default();
    let device_lost = Arc::new(AtomicBool::new(false));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("EffectCraft").with_app_id("ai.storyteller.effectcraft"),
        // Required on Android: eframe hands this to `EventLoopBuilderExtAndroid::with_android_app`
        // and returns an error without it.
        android_app: Some(app),
        ..Default::default()
    };

    let result = eframe::run_native(
        "EffectCraft",
        options,
        Box::new({
            let notices = notices.clone();
            let device_lost = device_lost.clone();
            move |cc| {
                // The window's device reports its own loss; the app closes itself for it (see
                // `close_after_device_loss`).
                let gpu_failures = effectcraft_ui_egui::gpu_failure::GpuFailureBridge::new(&cc.egui_ctx);
                if let Some(rs) = &cc.wgpu_render_state {
                    log::info!("graphics: {:?}", rs.adapter.get_info());
                    let errors = gpu_failures.clone();
                    rs.device.on_uncaptured_error(Arc::new(move |error| {
                        let message = format!("uncaptured GPU error: {error}");
                        if errors.report(&message, false) {
                            log::error!("{message}");
                        }
                    }));
                    let lost = gpu_failures.clone();
                    rs.device.set_device_lost_callback(move |reason, message| {
                        lost.report(&format!("GPU device lost ({reason:?}): {message}"), true);
                    });
                } else {
                    log::warn!("graphics: no wgpu device");
                }
                let mut session = effectcraft_host::session();
                // Lazy open and non-blocking auto-save: footage is checked and auto-saves written on
                // background threads.
                session.check_footage_on_open = true;
                session.autosave.background = true;
                // Settings, shortcut presets, Roto Brush/Face Tracking models and the crash-recovery
                // sentinel live in the app's own data directory.
                session.config = Some(config_store());
                session.load_settings();
                let recovery = session.begin_recovery();
                // A relative render output has nowhere to go on Android without this.
                if session.prefs.export.default_output_folder.trim().is_empty()
                    && let Some(dir) = default_output_folder()
                {
                    session.prefs.export.default_output_folder = dir;
                    session.prefs_changed();
                }
                let mut app = EffectcraftApp::new(session);
                app.set_gpu_failure_bridge(gpu_failures);
                app.ui.start_screen = app.session.prefs.startup.show_home_on_launch;
                if let Some(r) = recovery {
                    app.offer_recovery(r);
                }
                // The ScriptUI panels that were open last time.
                let _ = app.session.execute("window.restoreScriptPanels", serde_json::json!({}));
                app.hooks = android_services(cc.egui_ctx.clone(), notices.clone(), device_lost.clone());
                Ok(Box::new(Shell {
                    inner: app,
                    notices,
                    device_lost,
                    modifiers: modifiers::Tracked::default(),
                    prefs_mirror: prefs_watch::Writer::new(app_data_dir().map(|d| d.join("prefs.json")), PREFS_EVERY),
                }))
            }
        }),
    );

    match result {
        Ok(()) => log::info!("EffectCraft exited normally"),
        Err(e) => log::error!("EffectCraft stopped: {e}"),
    }
}

/// Wraps the real app so the platform can act on the frame around it: the picker, the reports of
/// work that failed off the frame, and the end of the process when the user quits.
///
/// The same shape as the desktop shell (`apps/effectcraft/src/main.rs`, whose `Desktop` struct
/// forwards exactly these four methods to its `EffectcraftApp`) and the web build.
struct Shell {
    inner: EffectcraftApp,
    notices: Notices,
    /// wgpu reported the device lost: the process ends at the end of the frame.
    device_lost: Arc<AtomicBool>,
    /// Modifier keys, tracked here because the platform never reports them (see [`modifiers`]).
    modifiers: modifiers::Tracked,
    /// The settings, mirrored while the app runs rather than only on the way out (see
    /// [`prefs_watch`]).
    prefs_mirror: prefs_watch::Writer,
}

impl Shell {
    /// Everything the host has to do before the process ends: what `on_exit` would do (the app's own
    /// workspace save and the clean-exit sentinel), the settings mirror, the log flush.
    ///
    /// A process exit runs no `on_exit`, so the app's is called by hand — that is the only place the
    /// recovery sentinel is cleared, and a stale sentinel would make the next launch offer to recover
    /// a project that was in fact closed cleanly.
    fn finish(&mut self) {
        // A dialog that is open when the process ends is answered first; nothing is left waiting on
        // the other side of it.
        file_picker::cancel();
        eframe::App::on_exit(self);
        // A distinctive line, so a build can be checked for this path: `std::process::exit` is not
        // inspectable in the artifact, but a string this code emits is.
        log::info!("File > Exit: ending the process");
        // The file logger writes each record as it arrives, but a logger that buffered would lose its
        // last lines here: an exit runs no flush of its own.
        log::logger().flush();
        std::process::exit(0);
    }

    /// File ▸ Exit, and the answer to the unsaved-changes prompt.
    ///
    /// On Android `ViewportCommand::Close` ends nothing: `egui-winit` turns it into a plain
    /// `ViewportEvent::Close` (it never calls winit's window API, which is stubbed out on Android
    /// anyway), so the app sits on a stale close request, dies on the next input event, and then
    /// **cannot be reopened until it is cleared from Recents**. So the app's own `quit_requested`
    /// flag — set by the Quit command once no project is modified, and by the unsaved-changes prompt
    /// once it is answered — ends the process instead.
    fn quit_if_asked(&mut self) {
        if !self.inner.quit_requested {
            return;
        }
        self.inner.quit_requested = false;
        self.finish();
    }

    /// The graphics device is gone: say why in the log, then end the frame and the process.
    ///
    /// The app has already auto-saved what was unsaved and asked for the window to close, which on
    /// Android does nothing — so without this the app would sit on a dead device, drawing nothing,
    /// forever.
    fn close_after_device_loss(&mut self) {
        if !self.device_lost.swap(false, Ordering::Relaxed) {
            return;
        }
        self.finish();
    }

    /// Anything the picker could not do, said out loud, one at a time.
    fn show_notices(&mut self, ctx: &egui::Context) {
        let mut pending = self.notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(text) = pending.pop() else {
            return;
        };
        pending.truncate(1);
        drop(pending);
        egui::Window::new("EffectCraft")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(10.0, 8.0);
                ui.label(text);
                if ui.button("OK").clicked() {
                    *self.notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Vec::new();
                }
            });
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.inner.logic(ctx, frame);
        // The settings, written as the app runs: Android never gives a clean exit moment.
        if self.prefs_mirror.due() {
            let bytes = prefs_bytes(&self.inner);
            self.prefs_mirror.store(&bytes);
        }
        self.close_after_device_loss();
        self.quit_if_asked();
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Before the app, so the frame it sees already has the modifiers a desktop backend would have
        // reported (see `modifiers`). Without this every Ctrl/Shift/Alt shortcut and Shift+click are
        // dead on Android, silently.
        self.modifiers.apply(raw);
        self.inner.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
        let ctx = ui.ctx().clone();
        // After the app, so the picker and the notices float above the editor.
        file_picker::draw(&ctx);
        self.show_notices(&ctx);
        // A menu click sets these during this call: check them here, in the same frame, rather than
        // leaving it to the next `logic` — no repaint is guaranteed after a click, and waiting for one
        // is how File ▸ Exit ends up doing nothing until the user clicks again.
        self.close_after_device_loss();
        self.quit_if_asked();
    }

    fn on_exit(&mut self) {
        let bytes = prefs_bytes(&self.inner);
        self.prefs_mirror.store_now(&bytes);
        self.inner.on_exit();
    }
}
