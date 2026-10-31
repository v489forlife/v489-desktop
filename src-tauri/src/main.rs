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
/// Desktop Discord sign-in: the app never signs in inside the webview. It asks the running Discord client (local IPC
/// AUTHORIZE); if that fails it opens the system browser. Either way the site mints a one-time code bound to our random `state`, and returns via v489://auth.
#[derive(Default)]
struct AuthState(std::sync::Mutex<Option<String>>);
const SITE: &str = "https://v489.app";

fn is_login_nav(u: &url::Url) -> bool {
    let h = u.host_str().unwrap_or("");
    ((h == "discord.com" || h.ends_with(".discord.com")) && u.path().contains("/oauth2/authorize")) || (h == "v489.app" && u.path().starts_with("/api/auth/signin/discord"))
}

/// Overlay shown while a sign-in is pending. Always has Cancel + Try again and a 2-minute timeout, so the page can never get stuck.
fn login_overlay(app: &AppHandle, msg: &str) {
    let Some(w) = app.get_webview_window("main") else { return };
    let js = r#"(function(M){clearTimeout(window.__v489t);var d=document.getElementById('v489-login')||document.createElement('div');d.id='v489-login';d.style.cssText='position:fixed;inset:0;z-index:2147483647;display:grid;place-items:center;background:rgba(5,5,7,.94);color:#d8d8e4;font:15px system-ui;text-align:center';
var B='margin:8px 4px 0;border:0;border-radius:8px;padding:9px 16px;cursor:pointer;font:600 14px system-ui;';
d.innerHTML='<div><div style="font:900 26px system-ui;letter-spacing:.3em;color:#ff2e4d">V489</div><p id=v489m></p><p style="color:#6a6a7c;font-size:12px">Nothing happening? Hit Try again.</p><button id=v489r style="'+B+'background:#5865F2;color:#fff">Try again</button><button id=v489c style="'+B+'background:#222;color:#ddd">Cancel</button></div>';
if(!d.parentNode)document.body.appendChild(d);d.querySelector('#v489m').textContent=M;
d.querySelector('#v489r').onclick=function(){location.href='https://v489.app/__v489_login'};
d.querySelector('#v489c').onclick=function(){clearTimeout(window.__v489t);location.href='https://v489.app/__v489_cancel'};
window.__v489t=setTimeout(function(){var m=document.getElementById('v489m');if(m)m.textContent='Sign-in timed out. Try again?'},120000);})("#;
    let _ = w.eval(&format!("{js}{});", serde_json::to_string(msg).unwrap_or_default()));
}

/// Every click starts a FRESH login: new state replaces any pending one (late results from an old attempt are ignored).
fn start_login(app: &AppHandle) {
    let state = uuid::Uuid::new_v4().simple().to_string();
    *app.state::<AuthState>().0.lock().unwrap() = Some(state.clone());
    login_overlay(app, "Approve V489 in your Discord app…");
    let a = app.clone();
    std::thread::spawn(move || {
        match discord_ipc::authorize(&a, &state) {
            Ok(code) => {
                if a.state::<AuthState>().0.lock().unwrap().as_deref() != Some(state.as_str()) { return; }
                // Exchange happens server-side; the Discord code goes in a same-origin POST body, never a URL.
                if let Some(w) = a.get_webview_window("main") {
                    let js = format!(r#"(function(c,s){{fetch('/api/desktop-auth/rpc',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({{code:c,state:s}})}}).then(function(r){{return r.ok?r.json():Promise.reject()}}).then(function(j){{location.replace('/desktop-auth/redeem#code='+encodeURIComponent(j.code)+'&state='+encodeURIComponent(s))}}).catch(function(){{location.href='https://v489.app/__v489_browser'}})}})({},{});"#, serde_json::to_string(&code).unwrap(), serde_json::to_string(&state).unwrap());
                    let _ = w.eval(&js);
                }
            }
            Err(_) => {
                if a.state::<AuthState>().0.lock().unwrap().as_deref() != Some(state.as_str()) { return; }
                browser_login(&a, &state);
            }
        }
    });
}

/// Fallback: system browser (Discord web) -> one-time code -> v489://auth.
fn browser_login(app: &AppHandle, state: &str) {
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(format!("{SITE}/api/desktop-auth/start?state={state}"), None::<&str>);
    login_overlay(app, "Discord app not available. Finish signing in in your browser, then come back here.");
}

fn fallback_to_browser(app: &AppHandle) {
    let st = app.state::<AuthState>().0.lock().unwrap().clone();
    match st { Some(s) => browser_login(app, &s), None => start_login(app) }
}

fn cancel_login(app: &AppHandle) {
    *app.state::<AuthState>().0.lock().unwrap() = None;
    if let Some(w) = app.get_webview_window("main") { if let Ok(u) = SITE.parse() { let _ = w.navigate(u); } }
}

/// Discord local IPC (\\.\pipe\discord-ipc-N): handshake + AUTHORIZE shows the authorize modal inside the Discord client.
mod discord_ipc {
    use std::io::{Read, Write};
    use tauri::AppHandle;
    fn frame(op: u32, v: &serde_json::Value) -> Vec<u8> { let b = v.to_string().into_bytes(); let mut o = op.to_le_bytes().to_vec(); o.extend((b.len() as u32).to_le_bytes()); o.extend(b); o }
    fn read(p: &mut std::fs::File) -> Result<(u32, serde_json::Value), String> {
        let mut h = [0u8; 8]; p.read_exact(&mut h).map_err(|e| e.to_string())?;
        let op = u32::from_le_bytes(h[0..4].try_into().unwrap()); let n = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        if n > 1 << 20 { return Err("frame".into()); }
        let mut b = vec![0u8; n]; p.read_exact(&mut b).map_err(|e| e.to_string())?;
        Ok((op, serde_json::from_slice(&b).map_err(|e| e.to_string())?))
    }
    fn client_id() -> Result<String, String> {
        let r = ureq::get(&format!("{}/api/desktop-auth/rpc", super::SITE)).call().map_err(|e| e.to_string())?;
        let j: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
        j["clientId"].as_str().map(String::from).ok_or("no client id".into())
    }
    #[cfg(windows)]
    fn open() -> Result<std::fs::File, String> {
        for i in 0..10 { if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) { return Ok(f); } }
        Err("discord not running".into())
    }
    #[cfg(not(windows))]
    fn open() -> Result<std::fs::File, String> { Err("unsupported".into()) }
    pub fn authorize(_app: &AppHandle, state: &str) -> Result<String, String> {
        let mut p = open()?; let cid = client_id()?;
        p.write_all(&frame(0, &serde_json::json!({"v":1,"client_id":cid}))).map_err(|e| e.to_string())?;
        let (_, ready) = read(&mut p)?; if ready["evt"] != "READY" { return Err(format!("handshake: {ready}")); }
        let nonce = uuid::Uuid::new_v4().to_string();
        p.write_all(&frame(1, &serde_json::json!({"cmd":"AUTHORIZE","nonce":nonce,"args":{"client_id":cid,"scopes":["identify","guilds","guilds.members.read"],"state":state}}))).map_err(|e| e.to_string())?;
        loop { // blocks while the user looks at the modal; a stale result is discarded by the caller's state check
            let (op, m) = read(&mut p)?;
            if op == 2 { return Err("closed".into()); }
            if m["nonce"] != nonce.as_str() { continue; }
            if m["evt"] == "ERROR" { return Err(m["data"]["message"].as_str().unwrap_or("error").into()); }
            return m["data"]["code"].as_str().map(String::from).ok_or("no code".into());
        }
    }
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

static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

/// Runs right before the updater spawns the NSIS installer and calls process::exit(0).
/// Tear down the tray, every webview (WebView2 child processes) and the audio loop so nothing is
/// still holding V489.exe when the installer checks for running instances ("Failed to kill" fix).
fn before_update_exit() {
    let Some(app) = APP.get() else { return };
    STOP_AUDIO.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = app.remove_tray_by_id("main");
    for (_, w) in app.webview_windows() { let _ = w.destroy(); }
    std::thread::sleep(std::time::Duration::from_millis(400));
}

/// Install the pending update (NSIS passive) and relaunch. No-op if none.
fn apply_update(app: &AppHandle) -> bool {
    let p = app.state::<Pending>().0.lock().unwrap().take();
    if let Some((u, bytes)) = p { before_update_exit(); let _ = u.install(bytes); app.restart(); }
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
static STOP_AUDIO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        if let Ok(s) = stream { let _ = s.play(); while !STOP_AUDIO.load(Ordering::SeqCst) { std::thread::park_timeout(std::time::Duration::from_millis(200)); } drop(s); }
    });
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        if STOP_AUDIO.load(Ordering::SeqCst) { break; }
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
        .plugin(tauri::plugin::Builder::<tauri::Wry, ()>::new("v489-update-nav").on_navigation(|w, url| { if w.label() == "main" && is_login_nav(url) { start_login(w.app_handle()); return false; } if w.label() == "main" && url.as_str().starts_with("https://v489.app/__v489_") && !url.as_str().starts_with(UPDATE_URL) { let a = w.app_handle(); match url.path() { "/__v489_login" => start_login(a), "/__v489_cancel" => cancel_login(a), "/__v489_browser" => fallback_to_browser(a), _ => {} } return false; } if url.as_str().starts_with(UPDATE_URL) { let a = w.app_handle().clone(); std::thread::spawn(move || { apply_update(&a); }); return false; } true }).build())
        .on_page_load(|w, _| { if w.label() == "main" { show_update_btn(w.app_handle()); } })
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--tray"])))
        .setup(|app| {
            // Main window is built here (create:false in tauri.conf) so it gets a download handler:
            // WebView2 shows nothing for downloads in this host, so "Download sample" looked dead.
            if let Some(cfg) = app.config().app.windows.iter().find(|w| w.label == "main").cloned() {
                WebviewWindowBuilder::from_config(app, &cfg)?.on_download(|wv, ev| {
                    match ev {
                        tauri::webview::DownloadEvent::Requested { destination, .. } => {
                            let name = destination.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "V489-download".into());
                            if let Ok(dir) = wv.app_handle().path().download_dir() {
                                let mut p = dir.join(&name);
                                let (stem, ext) = (p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default());
                                let mut i = 1; while p.exists() { p = dir.join(format!("{stem} ({i}){ext}")); i += 1; }
                                *destination = p;
                            }
                        }
                        tauri::webview::DownloadEvent::Finished { path, success, .. } => {
                            if success { if let Some(p) = path { let _ = tauri_plugin_opener::reveal_item_in_dir(p); } }
                        }
                        _ => {}
                    }
                    true
                }).build()?;
            }
            let h = app.handle().clone();
            let _ = APP.set(h.clone());
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
                    "quit" => { if !apply_update(app) { app.exit(0) } }
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
