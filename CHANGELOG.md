# Changelog

## v0.1.0 — first published port (EffectCraft 0.8.0)

| | |
|---|---|
| APK | `effectcraft-0.8.0-arm64-v8a.apk` (30.1 MB) |
| sha256 | `466cfa9e2438c1a148389564fc47c0d395ccfdb951fa21f0a148a25877d1100a` |
| Built from | upstream `storytold/effectcraft` @ `0b2d85b8111832a151c867f5e8a355965762b179` + `patches/0001-effectcraft-android-port.patch` + `apps/effectcraft-android/` |
| Package | `ai.storyteller.effectcraft` — versionName `0.8.0`, minSdk 26, targetSdk 35 |
| ABI | `arm64-v8a` only |
| Signature | throwaway local test key, not an official key |

### In this release

- Android shell crate: NativeActivity entry point, a `Hooks` implementation filling the app's own
  asynchronous dialog seam, a settings store and log in the app's own data directory, preferences
  mirrored while the app runs (atomic), modifier-key synthesis, an Activity that is not recreated on
  configuration change, launcher icon and the edge-to-edge theme variant.
- **The file dialogs work** — Import, Save As, every export, Collect Files, Relink Missing Footage and
  the rest go through `file_dialog_async`. The blocking slots stay `None` on purpose, so the panels
  that reach one take their written fallbacks instead of drawing a button that does nothing.
- **`File ▸ Export ▸ Adobe Premiere Pro Project…` works again**: it had no dialog route at all and
  failed with *"missing `path`"* — a desktop bug this port turned up and fixes.
- **`File ▸ Exit` ends the process** — and asks first: a modified project raises the app's own Unsaved
  Changes prompt, and only the answer to it quits. Without the new `quit_requested` flag the app sat
  on a stale `ViewportCommand::Close`, which on Android ends nothing and leaves the app looking frozen
  and unopenable until it is cleared from Recents.
- **System fonts** load from `/system/fonts` (and `/product/fonts`, `/system_ext/fonts`,
  `/vendor/fonts`); before, the scan took the Linux branch and the app had none.
- **Renders and exports have a writable destination**: the default output folder is the app's own
  external files directory, since on Android `HOME` is unset and the working directory is `/`.

### Known issues

See *Known gaps* in the [README](README.md) and [`docs/PORT-NOTES.md`](docs/PORT-NOTES.md).
