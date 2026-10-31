# V489 Desktop (beta)

Tauri 2 wrapper for https://v489.app plus a live **Studio wallpaper**.

- Main window = the site (log in once; cookies persist).
- Tray: *Studio wallpaper* (parents a borderless window to the desktop WorkerW, behind icons), *Floating studio* (always on top), *Start with Windows*, Quit. Closing the main window hides it to the tray.
- PC audio: WASAPI loopback (cpal) → `pc-audio` events (`{bass, beat}` ~30/s) → `/studio-wall` pumps speakers/lights.
- The site only gets `event.listen` permission for `https://v489.app/*`. No other native API is exposed.

## Build
Push a tag `v0.1.0` to GitHub; `.github/workflows/release.yml` builds the unsigned NSIS installer on `windows-latest` and attaches it to a GitHub Release.
Local (on Windows): `npm install && npx tauri build`.
