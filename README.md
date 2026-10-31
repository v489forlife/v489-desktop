<p align="center"><a href="https://github.com/v489forlife/v489-desktop/releases/latest/download/V489-Setup.exe"><img alt="Download V489 for Windows" src="https://img.shields.io/badge/%E2%AC%87%20DOWNLOAD%20V489%20FOR%20WINDOWS-ff2e4d?style=for-the-badge&logoColor=white" height="56"></a></p>

<p align="center"><b><a href="https://github.com/v489forlife/v489-desktop/releases/latest/download/V489-Setup.exe">⬇ Download V489-Setup.exe</a></b> — always the latest version. Auto-updates itself after install.<br><sub>Beta, not code-signed yet: if Windows SmartScreen warns, click <i>More info</i> → <i>Run anyway</i>.</sub></p>

# V489 Desktop (beta)

Tauri 2 wrapper for https://v489.app plus a live **Studio wallpaper**.

- Main window = the site (log in once; cookies persist).
- Tray: *Studio wallpaper* (parents a borderless window to the desktop WorkerW, behind icons), *Floating studio* (always on top), *Start with Windows*, Quit. Closing the main window hides it to the tray.
- PC audio: WASAPI loopback (cpal) → `pc-audio` events (`{bass, beat}` ~30/s) → `/studio-wall` pumps speakers/lights.
- The site only gets `event.listen` permission for `https://v489.app/*`. No other native API is exposed.

## Build
Push a tag `vX.Y.Z` (bump version in tauri.conf.json) to GitHub; `.github/workflows/release.yml` builds the unsigned NSIS installer on `windows-latest` and attaches it to a GitHub Release.
Local (on Windows): `npm install && npx tauri build`.

## Auto-updates
The app checks GitHub Releases (`latest.json`, signed with the Tauri updater key) on launch and every 30 min, downloads in the background, and installs on restart/quit. When an update is ready an **Update ready, restart** button appears in the app and in the tray. The site itself is live, so site changes are instant; updates are only for the desktop shell.
