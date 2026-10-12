# EffectCraft for Android (arm64-v8a)

An unofficial Android port of **[EffectCraft](https://github.com/storytold/effectcraft)** — the real
engine and egui interface, packaged as a NativeActivity APK for sideloading on arm64 devices.

| | |
|---|---|
| Upstream project | [`storytold/effectcraft`](https://github.com/storytold/effectcraft) — Apache-2.0 OR MIT |
| Pinned commit | `0b2d85b81` (upstream version `0.8.0`) |
| Android package | `ai.storyteller.effectcraft` |
| Published APK | `effectcraft-0.8.0-arm64-v8a.apk` — 30.1 MB, arm64-v8a, signed |
| APK sha256 | `466cfa9e2438c1a148389564fc47c0d395ccfdb951fa21f0a148a25877d1100a` |
| Upstream files changed | 5 |
| New port crate | `apps/effectcraft-android/` — about 1 350 lines of Rust plus resources |

## What this repository is

This is **not a fork** of EffectCraft. It holds only the Android layer, so it stays small, readable,
and obviously separable from upstream:

```
apps/effectcraft-android/   the new shell crate — copy it into your checkout of upstream
patches/                    the diff against upstream — apply it after checking out the pin
docs/PORT-NOTES.md          what was changed and why, fix by fix, with how each one was checked
scripts/                    cross-compile environment, build and verify scripts (from shared-android-port)
CHANGELOG.md                what is in the published APK
```

The Android layer is not a reimplementation. It is a shell crate that fills the same `Hooks` struct the
desktop binary fills, so the real engine, the real render pipeline and the real egui UI run unchanged.

## Build it

Prerequisites: Rust (rustup) with the `aarch64-linux-android` target, Android SDK + NDK r27, a
JDK 21, and `cargo install cargo-apk`.

```bash
git clone https://github.com/storytold/effectcraft.git
cd effectcraft
git fetch --depth 1 origin 0b2d85b8111832a151c867f5e8a355965762b179
git checkout FETCH_HEAD                        # the exact commit this port targets
git apply /path/to/0001-effectcraft-android-port.patch
cp -r /path/to/effectcraft-android apps/       # the shell crate from this repository

# any keystore of your own signs an installable APK; a throwaway test key is fine for sideloading
keytool -genkeypair -keystore ks.jks -alias effectcraft -keyalg RSA -keysize 2048 -validity 10000 \
        -storepass android -keypass android -dname "CN=Local Test, O=Local, C=US"
export CARGO_APK_RELEASE_KEYSTORE="$PWD/ks.jks" CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
cargo apk build --release -p effectcraft-android
```

`scripts/android-env.sh` is the cross-compile environment this port was built with (the NDK linker
and the C compiler the native dependencies need, and the x86_64 linker for an emulator build),
`scripts/build-apk.sh` wraps the build, and `scripts/verify-apk.sh` checks the artifact:
`aapt2 dump badging`, `apksigner verify`, and `llvm-nm` on the `.so` inside the APK.

## Install

```bash
adb install -r effectcraft-0.8.0-arm64-v8a.apk
```

or copy the APK to the device and open it (installing from unknown sources must be allowed).

On first run, grant **Settings ▸ Apps ▸ EffectCraft ▸ Special access ▸ All files access** if you want
the in-app picker to reach footage outside the app's own folders. Without it, Android 10+ refuses a
`read_dir` of shared storage by path, and the picker says so on those roots — naming the permission and
the Settings path — rather than showing them as empty.

## Verification status

| Claim | How it was checked |
|---|---|
| Installs and launches | `adb install -r`, then `am start -n ai.storyteller.effectcraft/android.app.NativeActivity`; the app's own `Logs/EffectCraft Log.txt` starts with `EffectCraft 0.8.0 starting on Android` and `logging enabled (android 0.8.0)` |
| The real editor runs | tapping *Open Demo Project* loaded the `EffectCraft Intro` composition: the **composition viewer renders a real composited frame** (the logo, the ring graphic, the tagline), the Project panel lists the comps and folders, and the timeline shows its 8 layers with parent links and clip bars |
| **The file picker works, end to end** | File ▸ *Import ▸ File…* opened the in-app picker above the editor (title `Import Files — Android`), offering the app's own folder, its USB-visible media folder and **its `Renders` folder**. `Download` listed both pushed files once "All files access" was granted; ticking `test-photo.jpg` and tapping *Place* put it in the Project panel as an Image. Nothing in the app's own dialog was reimplemented |
| **`File ▸ Exit` ends the process, and asks first** | File ▸ Exit on a *modified* project raised the app's own **Unsaved Changes** prompt (`Save changes to "Untitled Project.ecproj" before closing?`) — it does not kill the work silently. *Don't Save* then produced `File > Exit: ending the process`, an empty `pidof`, and a task gone from `dumpsys activity activities` |
| Preferences and the workspace layout survive | `prefs.json` (7.7 KB), `shortcuts.json` and `workspaces.json` are written into the app's own data directory while the app runs, and its own `Logs/EffectCraft Log.txt` holds no write failures |
| **The file dialogs reach a working route** | source and runtime: the shell registers the app's own asynchronous `file_dialog_async` seam, which `menus::file_dialog` consults first for every command that needs a path. The blocking slots stay `None` on purpose, and each panel that reaches one already has a written fallback (Settings says "type the path", the Render Queue falls back to its inline name template) |
| **`File ▸ Export ▸ Adobe Premiere Pro Project…` no longer fails** | source: it had no dialog route at all and died with *"missing `path`"*; it now asks with a Save dialog defaulting to the Final Cut Pro XML that Premiere Pro imports. This fixes the desktop build too |
| Fonts come from the device | source: the font scan gained its Android branch (`/system/fonts`, `/product/fonts`, `/system_ext/fonts`, `/vendor/fonts`); without it the app fell through to the Linux font tree, which does not exist on a device. The UI renders in the platform face |
| Renders have somewhere to go | source, and visible in the picker: the default output folder is the app's own external files directory — on Android `HOME` is unset and the working directory is `/`, which nothing can write, so every Render Queue render would have failed on a permission error |
| The artifact is what it claims | `aapt2 dump badging` → package `ai.storyteller.effectcraft`, `versionName 0.8.0`, `targetSdk 35`, `native-code: 'arm64-v8a'`, application icon present; `apksigner verify --verbose` → v2 and v3 true; `llvm-nm --dynamic --defined-only` on the `.so` inside the APK → `android_main` and `ANativeActivity_onCreate`; the injected `android.app.lib_name` is `effectcraft`, matching `libeffectcraft.so`; `configChanges=0x40001ff8` (cargo-apk's default is `0x4a0`) |

**Not verified:** the emulator has no compute shaders, so the app reported
`GPU compositor unavailable … no compute shaders` and rendered on the CPU — a real device's driver is
what that claim rests on. The picker's touch targets and scrolling, and how the quit path feels, are
only real on a tablet. Audio preview is silent (see below), so nothing about it was exercised.

## Known gaps

- **No audio.** `cpal` has no backend for this target here, so `Hooks::audio_device` and
  `audio_devices` stay `None`: preview and scrub are silent. The codecs and the render pipeline are
  unaffected.
- **No system clipboard.** Edit ▸ Paste falls back to egui's own text handling — text typed into an
  egui field and anything copied inside the app still work — but a string copied in another app is not
  offered.
- **No system browser or file manager**, so Help ▸ links that would open the website or Discord do
  nothing.
- **No stylus pressure or tilt.** A pen degrades to ordinary touch.
- **No system title bar**, as on Windows and Linux, so the app keeps its own menu bar and header.

## The rest of this set

- [DeckCraft](https://github.com/kanaaka/deckcraft-android)
- [FilmCraft](https://github.com/kanaaka/filmcraft-android)
- [GridCraft](https://github.com/kanaaka/gridcraft-android)
- [LightCraft](https://github.com/kanaaka/lightcraft-android)
- [PdfCraft](https://github.com/kanaaka/pdfcraft-android)
- [PhotoCraft](https://github.com/kanaaka/photocraft-android)
- [SoundCraft](https://github.com/kanaaka/soundcraft-android)
- [VectorCraft](https://github.com/kanaaka/vectorcraft-android)
- [WordCraft](https://github.com/kanaaka/wordcraft-android)
- [shared-android-port](https://github.com/kanaaka/shared-android-port) — the pieces every port shares

## Credits and licence

EffectCraft is © its authors at [`storytold/effectcraft`](https://github.com/storytold/effectcraft) and
is dual-licensed Apache-2.0 / MIT; both licence texts are kept here. The Android layer in this
repository is an unofficial modification by [@kanaaka](https://github.com/kanaaka), carrying the same
licence. Not affiliated with or endorsed by the upstream project. See
[`ATTRIBUTION.md`](ATTRIBUTION.md).
