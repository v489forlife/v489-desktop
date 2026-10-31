// V489 desktop: wraps v489.app, adds a Studio wallpaper (behind desktop icons via WorkerW) or an always-on-top
// floating Studio, both reacting to PC audio through WASAPI loopback. Tray menu + autostart toggle.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::sync::{atomic::{AtomicU32, Ordering}, Arc};
use tauri::{menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem}, tray::TrayIconBuilder, AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;

/// A downloaded-but-not-installed update (signed, verified by the updater plugin).
#[derive(Default)]
struct Pending(std::sync::Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>);
/// Desktop Discord sign-in: the app never signs in inside the webview. It opens the system browser (which can hand off
/// to the installed Discord app), the site mints a one-time code bound to our random `state`, and returns via v489://auth.
#[derive(Default)]
struct AuthState(std::sync::Mutex<Option<String>>);
const SITE: &str = "https://v489.app";

fn is_login_nav(u: &url::Url) -> bool {
    let h = u.host_str().unwrap_or("");
    ((h == "discord.com" || h.ends_with(".discord.com")) && u.path().contains("/oauth2/authorize")) || (h == "v489.app" && u.path().starts_with("/api/auth/signin/discord"))
}

fn start_browser_login(app: &AppHandle) {
    let state = uuid::Uuid::new_v4().simple().to_string();
    *app.state::<AuthState>().0.lock().unwrap() = Some(state.clone());
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(format!("{SITE}/api/desktop-auth/start?state={state}"), None::<&str>);
    if let Some(w) = app.get_webview_window("main") { let _ = w.eval(r#"(function(){var d=document.getElementById('v489-login')||document.createElement('div');d.id='v489-login';d.style.cssText='position:fixed;inset:0;z-index:2147483647;display:grid;place-items:center;background:rgba(5,5,7,.92);color:#d8d8e4;font:15px system-ui;text-align:center';d.innerHTML='<div><div style="font:900 26px system-ui;letter-spacing:.3em;color:#ff2e4d">V489</div><p>Finish signing in with Discord in your browser.</p><p style="color:#6a6a7c;font-size:12px">You'll come right back here.</p><button onclick="this.parentNode.parentNode.remove()" style="margin-top:8px;background:#222;color:#ddd;border:0;border-radius:8px;padding:8px 14px">Cancel</button></div>';document.body.appendChild(d)})();"#); }
}

fn handle_deep_link(app: &AppHandle, raw: &str) {
    let Ok(u) = url::Url::parse(raw) else { return };
    if u.scheme() != "v489" || u.host_str() != Some("auth") { return; }
    let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else { return };
    let want = app.state::<AuthState>().0.lock().unwrap().take();
    if want.as_deref() != Some(state.as_str()) { return; } // not a login we started: ignore (blocks login-CSRF)
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    if let Some(w) = app.get_webview_window("main") {
        if let Ok(dest) = format!("{SITE}/desktop-auth/redeem#code={}&state={}", enc(code), enc(state)).parse() { let _ = w.navigate(dest); }
        open_main(app);
    }
}

const UPDATE_URL: &str = "https://v489.app/__v489_update";
const UPDATE_BTN: &str = r#"(function(){if(document.getElementById('v489-upd'))return;var b=document.createElement('a');b.id='v489-upd';b.href='https://v489.app/__v489_update';b.textContent='\u21BB Update ready \u2014 restart';b.style.cssText='position:fixed;right:16px;bottom:16px;z-index:2147483647;background:#ff2e4d;color:#fff;font:600 13px system-ui;padding:10px 14px;border-radius:10px;box-shadow:0 6px 24px rgba(255,46,77,.5);text-decoration:none';document.body.appendChild(b);})();"#;

fn show_update_btn(app: &AppHandle) { if app.state::<Pending>().0.lock().unwrap().is_some() { if let Some(w) = app.get_webview_window("main") { let _ = w.eval(UPDATE_BTN); } } }

/// Install the pending update (NSIS passive) and relaunch. No-op if none.
fn apply_update(app: &AppHandle) -> bool {
    let p = app.state::<Pending>().0.lock().unwrap().take();
    if let Some((u, bytes)) = p { if u.install(bytes).is_ok() { app.restart(); } }
    false
}

/// Check now, then every 30 min. Downloads in the background; installs on restart/quit or when the user clicks Update.
fn start_updater(app: AppHandle) {
    tauri::async_runtime::spawn(async move { loop {
        let have = app.state::<Pending>().0.lock().unwrap().is_some();
        if !have { if let Ok(up) = app.updater() { if let Ok(Some(u)) = up.check().await { if let Ok(bytes) = u.download(|_, _| {}, || {}).await {
            *app.state::<Pending>().0.lock().unwrap() = Some((u, bytes)); show_update_btn(&app);
            if let Some(t) = app.tray_by_id("main") { let _ = t.set_tooltip(Some("V489 — update ready (restart to install)")); }
        } } } }
        tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
    } });
}

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
        .plugin(tauri_plugin_single_instance::init(|app, argv, _| { for a in argv.iter() { if a.starts_with("v489://") { handle_deep_link(app, &a); } } open_main(app); }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AuthState::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Pending::default())
        .plugin(tauri::plugin::Builder::<tauri::Wry, ()>::new("v489-update-nav").on_navigation(|w, url| { if w.label() == "main" && is_login_nav(url) { start_browser_login(w.app_handle()); return false; } if url.as_str().starts_with(UPDATE_URL) { let a = w.app_handle().clone(); std::thread::spawn(move || { apply_update(&a); }); return false; } true }).build())
        .on_page_load(|w, _| { if w.label() == "main" { show_update_btn(w.app_handle()); } })
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--tray"])))
        .setup(|app| {
            let h = app.handle().clone();
            let auto_on = app.autolaunch().is_enabled().unwrap_or(false);
            let open = MenuItem::with_id(app, "open", "Open V489", true, None::<&str>)?;
            let wall = CheckMenuItem::with_id(app, "wall", "Studio wallpaper (behind icons)", true, false, None::<&str>)?;
            let float = CheckMenuItem::with_id(app, "float", "Floating studio (always on top)", true, false, None::<&str>)?;
            let auto = CheckMenuItem::with_id(app, "autostart", "Start with Windows", true, auto_on, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let upd = MenuItem::with_id(app, "update", "Restart to update (if ready)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &wall, &float, &auto, &PredefinedMenuItem::separator(app)?, &upd, &quit])?;
            let (wall2, float2, auto2) = (wall.clone(), float.clone(), auto.clone());
            TrayIconBuilder::with_id("main").icon(app.default_window_icon().unwrap().clone()).tooltip("V489").menu(&menu)
                .on_menu_event(move |app, e| match e.id.as_ref() {
                    "open" => open_main(app),
                    "wall" => { let on = toggle_wall(app); let _ = wall2.set_checked(on); }
                    "float" => { let on = toggle_float(app); let _ = float2.set_checked(on); }
                    "autostart" => { let m = app.autolaunch(); let on = !m.is_enabled().unwrap_or(false); let _ = if on { m.enable() } else { m.disable() }; let _ = auto2.set_checked(m.is_enabled().unwrap_or(on)); }
                    "update" => { apply_update(app); }
                    "quit" => { apply_update(app); app.exit(0) }
                    _ => {}
                }).build(app)?;
            if std::env::args().any(|a| a == "--tray") { if let Some(w) = app.get_webview_window("main") { let _ = w.hide(); } }
            // intercept the injected Update button: navigation to UPDATE_URL restarts into the new version
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                #[cfg(windows)] let _ = app.deep_link().register_all();
                let h2 = h.clone(); app.deep_link().on_open_url(move |e| { for u in e.urls() { handle_deep_link(&h2, u.as_str()); } });
                if let Ok(Some(us)) = app.deep_link().get_current() { for u in us { handle_deep_link(&h, u.as_str()); } }
                for a in std::env::args() { if a.starts_with("v489://") { handle_deep_link(&h, &a); } }
            }
            start_updater(h.clone());
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
