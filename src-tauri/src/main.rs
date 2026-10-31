// V489 desktop: wraps v489.app, adds a Studio wallpaper (behind desktop icons via WorkerW) or an always-on-top
// floating Studio, both reacting to PC audio through WASAPI loopback. Tray menu + autostart toggle.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::sync::{atomic::{AtomicU32, Ordering}, Arc};
use tauri::{menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem}, tray::TrayIconBuilder, AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

const WALL_URL: &str = "https://v489.app/studio-wall";

#[derive(Clone, serde::Serialize)]
struct Level { bass: f32, beat: f32 }

fn open_main(app: &AppHandle) { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.unminimize(); let _ = w.set_focus(); } }

fn toggle_wall(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("wall") { let _ = w.close(); return false; }
    let Ok(w) = WebviewWindowBuilder::new(app, "wall", WebviewUrl::External(WALL_URL.parse().unwrap())).title("V489 Studio Wallpaper").decorations(false).skip_taskbar(true).resizable(false).build() else { return false };
    if let Ok(Some(m)) = w.current_monitor() { let _ = w.set_position(*m.position()); let _ = w.set_size(*m.size()); }
    #[cfg(windows)] unsafe { wallpaper::attach(&w); }
    true
}

fn toggle_float(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("float") { let _ = w.close(); return false; }
    WebviewWindowBuilder::new(app, "float", WebviewUrl::External(WALL_URL.parse().unwrap())).title("V489 Studio").inner_size(440.0, 320.0).always_on_top(true).skip_taskbar(true).build().is_ok()
}

/// WASAPI loopback (Windows): capture what the PC is playing, push bass level + beat pulses to the studio windows.
fn start_audio(app: AppHandle) {
    let bass = Arc::new(AtomicU32::new(0)); let beat = Arc::new(AtomicU32::new(0));
    let (b2, k2) = (bass.clone(), beat.clone());
    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let Some(dev) = host.default_output_device() else { return };
        let Ok(cfg) = dev.default_output_config() else { return };
        let ch = cfg.channels() as usize; let sr = cfg.sample_rate().0 as f32;
        let a = 1.0 - (-2.0 * std::f32::consts::PI * 140.0 / sr).exp(); // ~140 Hz one-pole low-pass
        let (mut lp, mut env, mut avg) = (0.0f32, 0.0f32, 0.0f32);
        let stream = dev.build_input_stream(&cfg.into(), move |data: &[f32], _: &cpal::InputCallbackInfo| {
            for fr in data.chunks(ch) { let s = fr.iter().sum::<f32>() / ch as f32; lp += a * (s - lp); let e = lp.abs(); env = if e > env { e } else { env * 0.9995 }; }
            avg = avg * 0.995 + env * 0.005; let lvl = (env * 3.0).min(1.0);
            b2.store(lvl.to_bits(), Ordering::Relaxed); if env > avg * 1.35 && lvl > 0.2 { k2.store(1.0f32.to_bits(), Ordering::Relaxed); }
        }, |e| eprintln!("audio: {e}"), None);
        if let Ok(s) = stream { let _ = s.play(); loop { std::thread::park(); } }
    });
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        let b = f32::from_bits(bass.load(Ordering::Relaxed)); let k = f32::from_bits(beat.swap(0, Ordering::Relaxed));
        for l in ["wall", "float", "main"] { if let Some(w) = app.get_webview_window(l) { let _ = w.emit("pc-audio", Level { bass: b, beat: k }); } }
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| open_main(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--tray"])))
        .setup(|app| {
            let h = app.handle().clone();
            let auto_on = app.autolaunch().is_enabled().unwrap_or(false);
            let open = MenuItem::with_id(app, "open", "Open V489", true, None::<&str>)?;
            let wall = CheckMenuItem::with_id(app, "wall", "Studio wallpaper (behind icons)", true, false, None::<&str>)?;
            let float = CheckMenuItem::with_id(app, "float", "Floating studio (always on top)", true, false, None::<&str>)?;
            let auto = CheckMenuItem::with_id(app, "autostart", "Start with Windows", true, auto_on, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &wall, &float, &auto, &PredefinedMenuItem::separator(app)?, &quit])?;
            let (wall2, float2, auto2) = (wall.clone(), float.clone(), auto.clone());
            TrayIconBuilder::new().icon(app.default_window_icon().unwrap().clone()).tooltip("V489").menu(&menu)
                .on_menu_event(move |app, e| match e.id.as_ref() {
                    "open" => open_main(app),
                    "wall" => { let on = toggle_wall(app); let _ = wall2.set_checked(on); }
                    "float" => { let on = toggle_float(app); let _ = float2.set_checked(on); }
                    "autostart" => { let m = app.autolaunch(); let on = !m.is_enabled().unwrap_or(false); let _ = if on { m.enable() } else { m.disable() }; let _ = auto2.set_checked(m.is_enabled().unwrap_or(on)); }
                    "quit" => app.exit(0),
                    _ => {}
                }).build(app)?;
            if std::env::args().any(|a| a == "--tray") { if let Some(w) = app.get_webview_window("main") { let _ = w.hide(); } }
            start_audio(h);
            Ok(())
        })
        .on_window_event(|w, e| { if let tauri::WindowEvent::CloseRequested { api, .. } = e { if w.label() == "main" { api.prevent_close(); let _ = w.hide(); } } })
        .run(tauri::generate_context!())
        .expect("error while running V489");
}

#[cfg(windows)]
mod wallpaper {
    use windows::core::w;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowExW, FindWindowW, SendMessageTimeoutW, SetParent, SMTO_NORMAL};

    unsafe extern "system" fn find(top: HWND, out: LPARAM) -> BOOL {
        if let Ok(shell) = FindWindowExW(top, HWND::default(), w!("SHELLDLL_DefView"), None) {
            if !shell.is_invalid() { if let Ok(ww) = FindWindowExW(HWND::default(), top, w!("WorkerW"), None) { *(out.0 as *mut HWND) = ww; } }
        }
        BOOL(1)
    }
    /// Classic WorkerW trick: ask Progman to spawn the wallpaper WorkerW, then parent our window to it (draws behind icons).
    pub unsafe fn attach(win: &tauri::WebviewWindow) {
        let Ok(raw) = win.hwnd() else { return }; let me = HWND(raw.0 as _);
        let Ok(progman) = FindWindowW(w!("Progman"), None) else { return };
        let _ = SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(0x1), SMTO_NORMAL, 1000, None);
        let _ = SendMessageTimeoutW(progman, 0x052C, WPARAM(0), LPARAM(0), SMTO_NORMAL, 1000, None);
        let mut worker = HWND::default(); let _ = EnumWindows(Some(find), LPARAM(&mut worker as *mut HWND as isize));
        if worker.is_invalid() { if let Ok(w2) = FindWindowExW(progman, HWND::default(), w!("WorkerW"), None) { worker = w2; } } // Windows 11 24H2 layout
        if !worker.is_invalid() { let _ = SetParent(me, worker); }
    }
}
