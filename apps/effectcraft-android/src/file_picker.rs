//! An in-app file picker for Android, drawn with egui — no Java, no native dialog.
//!
//! # Why this exists
//!
//! `rfd` has no Android backend (its `backend.rs` compiles no module at all for a target that is
//! neither Linux/BSD, macOS, Windows nor wasm32), and Android's own picker — the Storage Access
//! Framework — needs `startActivityForResult`, which `android-activity` 0.6 does not expose. So a
//! ported desktop app has no file dialogs at all: File ▸ Open, Save As and every Export are
//! silently treated as cancelled. This module draws the picker instead.
//!
//! # How it plugs in
//!
//! EffectCraft already has the seam this needs, and it is the one the macOS build uses for the
//! same reason (a modal dialog's nested event loop inside a frame is unsafe, #511):
//!
//! ```text
//! Hooks::file_dialog_async: Option<Box<dyn Fn(FileDialog, DialogDone)>>
//!     FileDialog = Open { exts, many } | OpenProject | Folder | Save { default, ext, project }
//!     DialogDone = Box<dyn FnOnce(Vec<String>) + Send>   // empty = cancelled
//! ```
//!
//! The app hands the request and a callback to the host and carries on; the host answers from a
//! later frame. [`request`] stores both, [`draw`] shows the window, and answering calls the
//! callback — `menus::dialog_answered` then runs the command that asked, with the path. The
//! app's own picker-answer channel does the rest, so nothing in `crates/ui-egui` had to be
//! rewritten for this: the commands already prefer `file_dialog_async` over the blocking
//! `pick_files`/`pick_save`/… slots when it is present.
//!
//! The blocking slots stay `None` on purpose. They must return a path *in line*, and a picker
//! this shell has to draw cannot: the frame has to end and come back. Every panel that reaches
//! one degrades the way it was written to for a host without a dialog (a status line saying to
//! type the path, or the Render Queue's inline name template), which is what the desktop does on
//! a machine with no `rfd` either.
//!
//! # One dialog per window, not per process
//!
//! The dialog in flight is a **thread-local**, not a global. The window is the unit here: two
//! windows would otherwise share one dialog, so the second's request would cancel the first's and
//! the file the user tapped could open in the other window.
//!
//! # Permissions
//!
//! Android 10+ forbids reading `/storage/emulated/0/Download` by path. Two routes exist and this
//! picker works with either, because it probes each root and only offers the ones it can actually
//! read:
//!
//! - **App folder** — `external_data_path()`, the app's own directory. Always readable and
//!   writable, no permission at all.
//! - **App media (USB)** — `<shared storage>/Android/media/<package>`, the one app-owned folder
//!   Android 11+ leaves visible over MTP, so a file can be copied in with a cable.
//! - **Shared storage** — Download / Pictures / DCIM / Documents. Needs "All files access"
//!   (`MANAGE_EXTERNAL_STORAGE`, granted by hand in Settings ▸ Apps ▸ EffectCraft ▸ Special
//!   access) or `READ_MEDIA_IMAGES`/`READ_MEDIA_VIDEO`/`READ_MEDIA_AUDIO` for reading media. All
//!   of them are declared in this crate's `Cargo.toml`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The app's external files directory, captured in `android_main`.
///
/// `<shared storage>/Android/data/<package>/files` — always readable and writable with no
/// permission, so it is both the picker's default root and the place Save falls back to.
pub static EXT_DIR: OnceLock<PathBuf> = OnceLock::new();

const SHARED_ROOT: &str = "/storage/emulated/0";

/// Called with the paths a dialog picked (empty: cancelled). This is the app's own
/// `Hooks::DialogDone`, spelled out here so the picker does not have to depend on the UI crate.
pub type Done = Box<dyn FnOnce(Vec<String>) + Send>;

/// Which dialog is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// One file to open or import.
    Open,
    /// Several files.
    OpenMany,
    /// A path to save to.
    Save,
    /// A folder.
    Folder,
}

/// The dialog being shown: what was asked, and how to answer it.
struct Pending {
    kind: Kind,
    title: String,
    /// Extensions the listing is filtered to (none: every file).
    extensions: Option<Vec<String>>,
    /// Where the answer goes.
    answer: Option<Done>,
}

/// One folder's entries, kept between frames.
struct Listing {
    dir: PathBuf,
    /// `Err` when the folder can't be read at all — shared storage without the permission.
    entries: Result<(Vec<PathBuf>, Vec<PathBuf>), String>,
}

/// The picker's state, held for the window showing it.
#[derive(Default)]
struct Shared {
    pending: Option<Pending>,
    /// Directory being browsed.
    dir: PathBuf,
    /// File name for a Save dialog.
    name: String,
    /// The save extension; the name gets it if the user does not type one.
    ext: String,
    /// A message to show inside the picker (usually a permission problem).
    note: Option<String>,
    /// Chosen files, for a multi-select Open.
    picked: Vec<PathBuf>,
    /// Whether the starting directory was already applied.
    started: bool,
    /// List every file, whatever the app's type filter says — the desktop dialogs' "All files (*)".
    /// A filter must never be able to hide a file the app can open.
    show_all: bool,
    /// The roots and whether each can be read, worked out when the picker opens or on Refresh:
    /// probing them every frame would be six `read_dir`s per frame.
    roots: Option<Vec<(String, PathBuf, bool)>>,
    /// The browsed folder, read once per visit rather than once per frame.
    listing: Option<Listing>,
}

thread_local! {
    /// The dialog *this* window is showing. One per thread; see the module docs.
    static PENDING: RefCell<Shared> = RefCell::new(Shared::default());
}

/// Every extension a filter list offers, or `None` when it offers none (then every file is
/// listed). Convenience for the call sites that build filters from the app's `FileDialog`.
pub fn extensions(filters: &[&[&str]]) -> Option<Vec<String>> {
    let exts: Vec<String> = filters.iter().flat_map(|e| e.iter().map(|s| (*s).to_string())).collect();
    (!exts.is_empty()).then_some(exts)
}

/// Queue a dialog. The answer — one or more paths, or empty for cancelled — goes to `done` from a
/// later frame's [`draw`].
///
/// A dialog already open in *this* window is answered "cancelled" first: the app can only use one
/// answer at a time, and leaving the old request waiting would strand it.
pub fn request(kind: Kind, title: &str, extensions: Option<Vec<String>>, suggested: Option<String>, ext: &str, done: Done) {
    PENDING.with(|slot| {
        let mut s = slot.borrow_mut();
        if let Some(prev) = s.pending.take()
            && let Some(answer) = prev.answer
        {
            let _ = answer(Vec::new());
        }
        s.pending = Some(Pending { kind, title: title.to_string(), extensions, answer: Some(done) });
        s.name = suggested.unwrap_or_default();
        s.ext = ext.to_string();
        s.picked.clear();
        s.started = false;
        s.note = None;
        s.show_all = false;
        // Re-probe and re-read for every dialog. Cached for the app's lifetime, a permission
        // granted in Settings — or a folder whose first read failed — would keep its old answer
        // until the app was killed, which looks exactly like the picker ignoring the grant.
        s.roots = None;
        s.listing = None;
    });
}

/// Whether this window has a dialog waiting to be drawn.
#[allow(dead_code)]
pub fn is_open() -> bool {
    PENDING.with(|slot| slot.borrow().pending.is_some())
}

/// Close this window's dialog, if any, answering "cancelled" — for a shell that is quitting while
/// one is open.
pub fn cancel() {
    PENDING.with(|slot| {
        if let Some(pending) = slot.borrow_mut().pending.take()
            && let Some(answer) = pending.answer
        {
            let _ = answer(Vec::new());
        }
    });
}

/// `<external storage>/Android/media/<package>`: the app's own media folder.
///
/// Derived from `external_data_path()` (`…/Android/data/<package>/files`) so the package name is
/// not written down twice.
///
/// This is the one folder that is both part of the app's own storage (**no permission**) and left
/// **visible over USB/MTP** by Android 11+ — `Android/data` is hidden there, so a file the user
/// copies into the app folder with a cable silently never arrives.
pub fn media_dir() -> Option<PathBuf> {
    media_dir_from(EXT_DIR.get()?)
}

/// The derivation, on its own so it can be pinned by a test: `…/Android/data/<package>/files`
/// → `…/Android/media/<package>`.
fn media_dir_from(external_files: &Path) -> Option<PathBuf> {
    let package_dir = external_files.parent()?;
    let android = package_dir.parent()?.parent()?;
    Some(android.join("media").join(package_dir.file_name()?))
}

/// Roots to offer, as (label, path).
fn roots() -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Some(d) = EXT_DIR.get() {
        v.push(("App folder".to_string(), d.clone()));
    }
    if let Some(m) = media_dir() {
        v.push(("App media (USB)".to_string(), m));
    }
    if let Some(d) = EXT_DIR.get().and_then(|d| d.parent()) {
        // Sibling of the app's own files, still owned by the app.
        v.push(("App storage".to_string(), d.to_path_buf()));
    }
    for (label, sub) in [("Download", "Download"), ("Pictures", "Pictures"), ("DCIM", "DCIM"), ("Documents", "Documents")] {
        v.push((label.to_string(), Path::new(SHARED_ROOT).join(sub)));
    }
    v.push(("Internal storage".to_string(), PathBuf::from(SHARED_ROOT)));
    v
}

/// The best directory to open first: the app folder is always writable, so it is the only safe
/// default — a desktop path arriving as a suggestion never exists on the device.
fn default_dir() -> PathBuf {
    EXT_DIR.get().cloned().unwrap_or_else(|| PathBuf::from(SHARED_ROOT))
}

fn readable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// Shared storage outside the app's own folders — `/storage/emulated/0/…` but not
/// `/storage/emulated/0/Android/…`.
///
/// This is where the OS lies to the app: without "All files access" a shared folder still
/// **opens** (`read_dir` succeeds, so [`readable`] says yes and no lock is shown), but its
/// **contents are filtered away** — an empty listing for a folder full of files. So an empty
/// listing here is not evidence of an empty folder, and the picker must say so.
fn is_shared_storage(dir: &Path) -> bool {
    let shared = Path::new(SHARED_ROOT);
    dir.starts_with(shared) && !dir.starts_with(shared.join("Android"))
}

/// Everything in `dir`, folders first, both sorted, hidden files left out.
///
/// Unfiltered on purpose: the type filter is applied in memory by [`keep`], so the whole folder is
/// read once per visit and the picker can still say how many files a filter is hiding.
fn entries(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in rd.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            dirs.push(path);
        } else {
            files.push(path);
        }
    }
    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()));
    files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()));
    Ok((dirs, files))
}

/// Whether the app's filter offers `path`. The picker lists the **union** of every extension the
/// app passed, so a format the app reads is never hidden behind another one's filter — `.psd` sits
/// beside `.mp4` and `.png` in File ▸ Import's list. `None` means every file, as does "Show all
/// files".
fn keep(path: &Path, extensions: Option<&Vec<String>>) -> bool {
    match extensions {
        None => true,
        Some(exts) => {
            let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
            ext.is_some_and(|e| exts.iter().any(|w| w.eq_ignore_ascii_case(&e)))
        }
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// `name` with the save dialog's extension, if it does not already end in it. `dir.join(name)` is
/// what a Save answers with, and the app's atomic write needs the parent folder to exist — so the
/// path built here is always inside the folder the user is looking at.
fn with_extension(name: &str, ext: &str) -> String {
    let ext = ext.trim().trim_start_matches('.');
    if ext.is_empty() {
        return name.to_string();
    }
    if Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        return name.to_string();
    }
    format!("{name}.{ext}")
}

/// Draw the picker if this window has a dialog queued. Called from the shell's `ui`, *after*
/// forwarding to the app's own `ui`, so the window floats above the editor.
pub fn draw(ctx: &egui::Context) {
    PENDING.with(|slot| {
        let mut s = slot.borrow_mut();
        let Some(kind) = s.pending.as_ref().map(|p| p.kind) else {
            return;
        };
        let title = s.pending.as_ref().map(|p| p.title.clone()).unwrap_or_default();
        // First frame: pick a starting directory.
        if !s.started {
            s.started = true;
            s.dir = default_dir();
            if kind == Kind::Save && s.name.is_empty() {
                s.name = "untitled".to_string();
            }
        }

        let mut answer: Option<Vec<String>> = None;
        let mut cancel = false;
        let mut navigate: Option<PathBuf> = None;
        // Set by Refresh: the caches must not be written back at the end of the frame.
        let mut refresh = false;

        let dir = s.dir.clone();
        // Probe the roots once, not six `read_dir`s every frame.
        if s.roots.is_none() {
            s.roots = Some(roots().into_iter().map(|(label, path)| {
                let ok = readable(&path);
                (label, path, ok)
            }).collect());
        }
        // Read the folder once per visit rather than once per frame: the type filter then runs
        // over memory, and the picker can say how many files it is hiding.
        if s.listing.as_ref().is_none_or(|l| l.dir != dir) {
            s.listing = Some(Listing { dir: dir.clone(), entries: entries(&dir) });
        }
        let roots_now = s.roots.clone().unwrap_or_default();
        // Read before the window: the file loop filters by it while mutating `s`.
        let show_all = s.show_all;
        // Moved out while the window draws, so the closure can hold these and `s` at once.
        let listing = s.listing.take();
        let exts = s.pending.as_ref().and_then(|p| p.extensions.clone());
        let multi = kind == Kind::OpenMany;
        let all_files: &[PathBuf] = match listing.as_ref().map(|l| &l.entries) {
            Some(Ok((_, files))) => files,
            _ => &[],
        };
        let listed_count = if kind == Kind::Folder {
            all_files.len()
        } else {
            all_files.iter().filter(|f| show_all || keep(f, exts.as_ref())).count()
        };
        let hidden = all_files.len() - listed_count;
        // A tablet screen is smaller than the desktop default, and in portrait narrower than a
        // hard-coded desktop width. `content_rect` is the area egui considers safe (it excludes an
        // OS status bar where the backend reports one).
        let screen = ctx.content_rect();
        let (w, h) = ((screen.width() * 0.92).min(720.0), (screen.height() * 0.82).min(620.0));

        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_width(w)
            .default_height(h)
            .default_pos(screen.center() - egui::vec2(w, h) / 2.0)
            .show(ctx, |ui| {
                // Finger-sized rows: the default spacing is sized for a mouse.
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.spacing_mut().button_padding = egui::vec2(10.0, 8.0);

                // Roots. A locked one stays tappable and says why: greyed out with a lock, it reads
                // as "your files aren't here" and gives no way forward.
                ui.horizontal_wrapped(|ui| {
                    for (label, path, ok) in roots_now.iter() {
                        let here = *path == dir;
                        let text = if *ok { label.clone() } else { format!("{label} \u{1F512}") };
                        if ui.add_enabled(!here, egui::Button::new(text)).clicked() {
                            if *ok {
                                navigate = Some(path.clone());
                            } else {
                                s.note = Some(format!(
                                    "{label} can't be read. Grant \u{201C}All files access\u{201D} in Settings \u{25B8} Apps \u{25B8} EffectCraft \u{25B8} Special access, then tap Refresh."
                                ));
                            }
                        }
                    }
                });
                ui.separator();

                // Current path + up
                ui.horizontal_wrapped(|ui| {
                    if ui.button("\u{2B06} Up").clicked()
                        && let Some(parent) = dir.parent()
                    {
                        navigate = Some(parent.to_path_buf());
                    }
                    if ui.button("\u{1F4C1} App folder").clicked() {
                        navigate = Some(default_dir());
                    }
                    // Grant the permission, come back and tap Refresh: the roots and the folder are
                    // probed again rather than keeping the answer from before the grant.
                    if ui.button("\u{21BB} Refresh").clicked() {
                        refresh = true;
                    }
                    ui.label(egui::RichText::new(dir.display().to_string()).monospace().small());
                    ui.label(egui::RichText::new(format!("({} files)", all_files.len())).weak().small());
                });

                // The type filter, and the way out of it. The desktop dialogs always offer
                // "All files"; without one, anything the app's filter list omits is invisible with no
                // hint that it exists.
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut s.show_all, "Show all files");
                    if hidden > 0 && !s.show_all {
                        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), format!("{hidden} file(s) hidden by the file-type filter"));
                    }
                });

                if let Some(note) = &s.note {
                    ui.colored_label(egui::Color32::from_rgb(220, 160, 60), note);
                }

                ui.separator();
                let list_h = (h - 210.0).max(120.0);
                egui::ScrollArea::vertical().max_height(list_h).show(ui, |ui| match listing.as_ref().map(|l| &l.entries) {
                    Some(Err(e)) => {
                        ui.label("Cannot read this folder.");
                        ui.label(egui::RichText::new(e.clone()).monospace().weak().small());
                        ui.label("Grant \"All files access\" in Settings \u{25B8} Apps \u{25B8} EffectCraft \u{25B8} Special access to browse shared storage.");
                    }
                    Some(Ok((dirs, files))) => {
                        if dirs.is_empty() && listed_count == 0 {
                            ui.label(if hidden > 0 {
                                format!("(no matching files \u{2014} {hidden} hidden by the file-type filter)")
                            } else {
                                "(no files visible here)".to_string()
                            });
                            // The commonest trap on a tablet: the folder really does have files, but
                            // the system is hiding its contents, so it reads as empty.
                            if hidden == 0 && is_shared_storage(&dir) {
                                ui.colored_label(
                                    egui::Color32::from_rgb(220, 160, 60),
                                    "If this folder has files that don't appear, grant \u{201C}All files access\u{201D} in Settings \u{25B8} Apps \u{25B8} EffectCraft \u{25B8} Special access, then tap Refresh. Without it the system hides shared folders' contents from EffectCraft.",
                                );
                            }
                        }
                        for d in dirs {
                            if ui.button(format!("\u{1F4C1} {}", file_label(d))).clicked() {
                                navigate = Some(d.clone());
                            }
                        }
                        if kind != Kind::Folder {
                            for f in files.iter().filter(|f| show_all || keep(f, exts.as_ref())) {
                                let chosen = s.picked.iter().any(|p| p == f);
                                let mark = if chosen { "\u{2611}" } else { "\u{2610}" };
                                let label = if multi { format!("{mark} {}", file_label(f)) } else { format!("   {}", file_label(f)) };
                                if ui.button(label).clicked() {
                                    if kind == Kind::Save {
                                        s.name = file_label(f);
                                    } else if multi {
                                        if chosen {
                                            s.picked.retain(|p| p != f);
                                        } else {
                                            s.picked.push(f.clone());
                                        }
                                    } else {
                                        answer = Some(vec![f.to_string_lossy().to_string()]);
                                    }
                                }
                            }
                        }
                    }
                    // No folder read yet.
                    None => {
                        ui.label("(reading the folder…)");
                    }
                });

                ui.separator();
                ui.horizontal_wrapped(|ui| match kind {
                    Kind::Save => {
                        ui.label("File name:");
                        ui.add(egui::TextEdit::singleline(&mut s.name).desired_width((w - 260.0).max(160.0)));
                        if ui.button("Save").clicked() {
                            let name = s.name.trim().to_string();
                            if name.is_empty() {
                                s.note = Some("Enter a file name.".to_string());
                            } else {
                                // A full path: the app's atomic write needs the parent to exist.
                                answer = Some(vec![dir.join(with_extension(&name, &s.ext)).to_string_lossy().to_string()]);
                            }
                        }
                    }
                    Kind::Folder => {
                        if ui.button("Choose this folder").clicked() {
                            answer = Some(vec![dir.to_string_lossy().to_string()]);
                        }
                    }
                    Kind::OpenMany => {
                        let n = s.picked.len();
                        let label = if n == 0 { "Place".to_string() } else { format!("Place {n} file(s)") };
                        if ui.add_enabled(n > 0, egui::Button::new(label)).clicked() {
                            answer = Some(s.picked.iter().map(|p| p.to_string_lossy().to_string()).collect());
                        }
                    }
                    Kind::Open => {
                        if ui.button("Open").clicked() {
                            // A file has to be tapped in the list above for an Open dialog.
                            s.note = Some("Tap a file to open it.".to_string());
                        }
                    }
                });
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }

                // Honest, and it corrects what the first build of this picker said: Android 11+
                // blocks copying into `Android/data` over USB/MTP, so "push files here" was wrong
                // advice.
                if dir == default_dir() {
                    ui.label(
                        egui::RichText::new(
                            "The app's own folder: files here open and save with no permission. Android 11+ blocks copying into Android/data over USB \u{2014} to bring files in, put them in Download and grant \u{201C}All files access\u{201D}.",
                        )
                        .weak()
                        .small(),
                    );
                    if let Some(media) = media_dir() {
                        ui.label(
                            egui::RichText::new(format!("Or use {} \u{2014} app storage that stays visible over USB.", media.display()))
                                .weak()
                                .small(),
                        );
                    }
                }
            });

        // Back into the cache; a navigation made it stale and the next frame re-reads it. Refresh
        // clears both caches instead of keeping this frame's answer, so the probe and the folder read
        // run again — that is the whole point of the button.
        if refresh {
            s.roots = None;
            s.listing = None;
            s.note = None;
        } else {
            s.listing = listing;
        }

        if let Some(dir) = navigate {
            s.dir = dir;
            s.note = None;
        }
        if let Some(paths) = answer {
            if let Some(pending) = s.pending.take()
                && let Some(done) = pending.answer
            {
                let _ = done(paths);
            }
            s.picked.clear();
        } else if cancel {
            // Cancelled: an empty answer is what the app reads as one.
            if let Some(pending) = s.pending.take()
                && let Some(done) = pending.answer
            {
                let _ = done(Vec::new());
            }
            s.picked.clear();
        }
        // The app only wakes the window about once a second while it waits, so the picker keeps
        // asking for frames itself — otherwise every tap would feel dead.
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder with the given files in it.
    fn scratch(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("effectcraft-android-picker-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    /// The picker lists the union of every filter the app's dialogs pass, so a format the app says
    /// it can read is never hidden behind another filter's list. `.psd` (a file the Import dialog
    /// lists) is pinned here beside a format it does not read.
    #[test]
    fn the_import_dialog_lists_every_extension_it_offers() {
        // The same list `menus::file_dialog` hands `FileDialog::Open { exts: MEDIA }`.
        const MEDIA: &[&str] = &[
            "mp4", "mov", "m4v", "mkv", "webm", "mxf", "png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "exr", "dpx", "cin", "wav", "aif", "aiff", "mp3", "flac",
            "ogg", "opus", "svg", "pdf", "ai", "eps", "psd", "psb", "tga", "hdr", "gltf", "glb", "obj", "stl", "fbx", "json", "csv", "tsv",
        ];
        let exts = extensions(&[MEDIA]).expect("the Import dialog offers extensions");
        for wanted in ["psd", "mp4", "png", "ai", "wav"] {
            assert!(exts.iter().any(|e| e == wanted), "\"{wanted}\" is not among the Import dialog's extensions");
        }

        let dir = scratch("import", &["plate.psd", "poster.PSD", "clip.mp4", "notes.txt"]);
        let (dirs, files) = entries(&dir).unwrap();
        let listed: Vec<String> = files.iter().filter(|f| keep(f, Some(&exts))).map(|p| file_label(p)).collect();
        assert!(dirs.is_empty(), "no subfolders were made");
        assert!(listed.contains(&"plate.psd".to_string()), "a .psd file the app can read must be listed: {listed:?}");
        assert!(listed.contains(&"poster.PSD".to_string()), "the extension match is case-insensitive: {listed:?}");
        assert!(listed.contains(&"clip.mp4".to_string()), "{listed:?}");
        // Not a format the app reads, so hidden — which is exactly why the picker says how many it
        // is hiding and offers "Show all files".
        assert!(!listed.contains(&"notes.txt".to_string()), "a format the app doesn't read is filtered out: {listed:?}");
        // The folder read is unfiltered, so the count of hidden files is knowable…
        assert!(files.iter().any(|p| file_label(p) == "notes.txt"), "the folder read still sees it: {files:?}");
        // …and "Show all files" reaches anything the filter would drop.
        assert!(keep(&dir.join("notes.txt"), None), "no filter offers every file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unreadable folder is reported, not silently empty — that is what "my files aren't there"
    /// looks like when the permission is missing.
    #[test]
    fn an_unreadable_folder_is_an_error_not_an_empty_one() {
        let missing = std::env::temp_dir().join("effectcraft-android-picker-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(entries(&missing).is_err(), "a folder that can't be read is an Err, so the picker can explain");
    }

    /// Hidden files stay out of the listing, as on the desktop.
    #[test]
    fn dotfiles_are_left_out() {
        let dir = scratch("hidden", &["shown.psd", ".hidden.psd"]);
        let (_, files) = entries(&dir).unwrap();
        let names: Vec<String> = files.iter().map(|p| file_label(p)).collect();
        assert_eq!(names, vec!["shown.psd".to_string()], "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Save answers with a full path inside the folder on screen, with the app's extension — the
    /// app's atomic write needs the parent folder to exist, and the path is what the document
    /// remembers (the title bar stops saying "Untitled Project").
    #[test]
    fn a_save_answers_with_a_full_path_carrying_the_extension() {
        let dir = Path::new("/storage/emulated/0/Download");
        assert_eq!(dir.join(with_extension("Scene", "ecproj")).to_string_lossy(), "/storage/emulated/0/Download/Scene.ecproj");
        assert_eq!(dir.join(with_extension("Scene.ecproj", "ecproj")).to_string_lossy(), "/storage/emulated/0/Download/Scene.ecproj");
        assert_eq!(dir.join(with_extension("Frame.PNG", "png")).to_string_lossy(), "/storage/emulated/0/Download/Frame.PNG");
        // No extension to add (File ▸ Watch Folder…, Collect Files…): the name is left alone.
        assert_eq!(dir.join(with_extension("Watch Folder", "")).to_string_lossy(), "/storage/emulated/0/Download/Watch Folder");
    }

    /// The USB-visible root's path: `Android/data` is hidden from MTP on Android 11+, so this
    /// folder is the one a user can actually copy a file into; the package name is derived rather
    /// than written down a second time, and this is what pins it.
    #[test]
    fn the_usb_visible_root_is_derived_from_the_app_folder() {
        let ext = Path::new("/storage/emulated/0/Android/data/ai.storyteller.effectcraft/files");
        assert_eq!(media_dir_from(ext), Some(PathBuf::from("/storage/emulated/0/Android/media/ai.storyteller.effectcraft")));
        assert_eq!(media_dir_from(Path::new("/nowhere")), None, "a path without the Android layout has no media folder");
    }

    /// Shared storage is where the OS hides a folder's contents; the app's own `Android/…` folders
    /// are not, so an empty listing there really is empty.
    #[test]
    fn shared_storage_is_recognised_so_an_empty_listing_can_be_explained() {
        for d in ["/storage/emulated/0/Documents", "/storage/emulated/0/Download", "/storage/emulated/0"] {
            assert!(is_shared_storage(Path::new(d)), "{d} is shared storage");
        }
        for d in [
            "/storage/emulated/0/Android/data/ai.storyteller.effectcraft/files",
            "/storage/emulated/0/Android/media/ai.storyteller.effectcraft",
            "/data/user/0/ai.storyteller.effectcraft/files",
        ] {
            assert!(!is_shared_storage(Path::new(d)), "{d} is the app's own");
        }
    }
}
