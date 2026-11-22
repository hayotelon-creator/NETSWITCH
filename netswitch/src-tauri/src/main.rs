#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod net;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Extra {
    pub ip: String,
    pub mask: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub iface: String,
    pub dhcp: bool,
    pub ip: String,
    pub mask: String,
    pub gateway: String,
    pub dns: Vec<String>,
    pub extra: Vec<Extra>,
    pub auto_on_link: bool,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Config {
    pub profiles: Vec<Profile>,
    pub auto: bool,
    pub last_applied: String,
}

struct AppState {
    cfg: Mutex<Config>,
    path: PathBuf,
}

impl AppState {
    fn save(&self) -> Result<(), String> {
        let json = {
            let c = self.cfg.lock().unwrap();
            serde_json::to_string_pretty(&*c).map_err(|e| e.to_string())?
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }
}

// ---------- Commandes appelées par l'interface ----------

#[tauri::command]
fn get_config(state: State<'_, Arc<AppState>>) -> Config {
    state.cfg.lock().unwrap().clone()
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<'_, Arc<AppState>>, cfg: Config) -> Result<(), String> {
    {
        *state.cfg.lock().unwrap() = cfg;
    }
    state.save()?;
    rebuild_tray_menu(&app);
    Ok(())
}

#[tauri::command]
async fn list_interfaces() -> Result<Vec<net::Iface>, String> {
    tauri::async_runtime::spawn_blocking(net::list_interfaces)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn apply_profile(app: AppHandle, index: usize) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || do_apply(&app, index))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn scan(iface: String) -> Result<Vec<net::Host>, String> {
    tauri::async_runtime::spawn_blocking(move || net::scan(&iface))
        .await
        .map_err(|e| e.to_string())?
}

// ---------- Logique ----------

fn do_apply(app: &AppHandle, index: usize) -> Result<String, String> {
    let st = app.state::<Arc<AppState>>();
    let profile = {
        let c = st.cfg.lock().unwrap();
        c.profiles.get(index).cloned()
    }
    .ok_or_else(|| "Profil introuvable".to_string())?;

    net::apply(&profile)?;

    {
        let mut c = st.cfg.lock().unwrap();
        c.last_applied = profile.name.clone();
    }
    let _ = st.save();
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("NetSwitch — {}", profile.name)));
    }
    rebuild_tray_menu(app);
    let _ = app.emit("applied", profile.name.clone());
    Ok(format!("Profil « {} » appliqué", profile.name))
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let cfg = {
        let st = app.state::<Arc<AppState>>();
        let c = st.cfg.lock().unwrap().clone();
        c
    };
    let menu = Menu::new(app)?;
    if cfg.profiles.is_empty() {
        menu.append(&MenuItem::with_id(app, "none", "Aucun profil", false, None::<&str>)?)?;
    }
    for (i, p) in cfg.profiles.iter().enumerate() {
        let label = if p.name == cfg.last_applied {
            format!("✓  {}", p.name)
        } else {
            format!("     {}", p.name)
        };
        menu.append(&MenuItem::with_id(app, format!("p:{i}"), label, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let auto_label = if cfg.auto {
        "Détection auto : activée"
    } else {
        "Détection auto : désactivée"
    };
    menu.append(&MenuItem::with_id(app, "auto", auto_label, true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "open", "Ouvrir NetSwitch…", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?)?;
    Ok(menu)
}

fn rebuild_tray_menu(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id("main"), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn report_error(app: &AppHandle, msg: String) {
    show_main(app);
    let _ = app.emit("error", msg);
}

/// Surveille les cartes réseau : quand un câble est branché (lien qui passe à "up"),
/// applique le profil marqué "auto" pour cette interface.
fn auto_loop(app: AppHandle) {
    let mut prev: HashMap<String, bool> = HashMap::new();
    let mut first = true;
    loop {
        std::thread::sleep(Duration::from_secs(4));
        let (auto, profiles) = {
            let st = app.state::<Arc<AppState>>();
            let c = st.cfg.lock().unwrap();
            (c.auto, c.profiles.clone())
        };
        if !auto {
            first = true;
            continue;
        }
        let ifaces = match net::list_interfaces() {
            Ok(v) => v,
            Err(_) => continue,
        };
        for i in &ifaces {
            let was_up = prev.get(&i.id).copied().unwrap_or(false);
            if !first && i.up && !was_up {
                if let Some(idx) = profiles.iter().position(|p| p.auto_on_link && p.iface == i.id) {
                    if let Err(e) = do_apply(&app, idx) {
                        report_error(&app, e);
                    }
                }
            }
            prev.insert(i.id.clone(), i.up);
        }
        first = false;
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let path = app.path().app_config_dir()?.join("config.json");
            let cfg: Config = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            app.manage(Arc::new(AppState { cfg: Mutex::new(cfg), path }));

            let menu = build_menu(app.handle())?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("NetSwitch")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref().to_string();
                    match id.as_str() {
                        "quit" => app.exit(0),
                        "open" => show_main(app),
                        "auto" => {
                            let st = app.state::<Arc<AppState>>();
                            {
                                let mut c = st.cfg.lock().unwrap();
                                c.auto = !c.auto;
                            }
                            let _ = st.save();
                            rebuild_tray_menu(app);
                            let _ = app.emit("config", ());
                        }
                        s if s.starts_with("p:") => {
                            if let Ok(i) = s[2..].parse::<usize>() {
                                let app = app.clone();
                                std::thread::spawn(move || {
                                    if let Err(e) = do_apply(&app, i) {
                                        report_error(&app, e);
                                    }
                                });
                            }
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            std::thread::spawn(move || auto_loop(handle));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_interfaces,
            apply_profile,
            scan
        ])
        .run(tauri::generate_context!())
        .expect("Erreur au lancement de NetSwitch");
}
