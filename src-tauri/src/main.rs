#![cfg_attr(windows, windows_subsystem = "windows")]
mod engine;
mod platform;
mod request;
use engine::{Engine, Settings};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_deep_link::DeepLinkExt;

fn show(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}
fn incoming(app: tauri::AppHandle, raw: String) {
    if [
        "home-service://",
        "home-service://open",
        "home-service://open/",
        "hometunnel://open",
    ]
    .contains(&raw.as_str())
    {
        show(&app);
        return;
    }
    let engine = app.state::<Engine>().inner().clone();
    tauri::async_runtime::spawn(async move {
        match request::Request::parse(&raw) {
            Ok(r) => {
                if let Err(error) = engine.submit(r).await {
                    engine.notice(error).await;
                }
            }
            Err(e) => {
                engine.notice(e).await;
                show(&app);
            }
        }
    });
}
#[tauri::command]
async fn snapshot(engine: tauri::State<'_, Engine>) -> Result<Vec<engine::View>, String> {
    Ok(engine.snapshot().await)
}
#[tauri::command]
async fn notices(engine: tauri::State<'_, Engine>) -> Result<Vec<engine::Notice>, String> {
    Ok(engine.notices().await)
}
#[tauri::command]
async fn get_settings(engine: tauri::State<'_, Engine>) -> Result<Settings, String> {
    Ok(engine.settings().await)
}
#[tauri::command]
async fn disconnect(engine: tauri::State<'_, Engine>, id: String) -> Result<(), String> {
    engine.disconnect(&id).await;
    Ok(())
}
#[tauri::command]
async fn disconnect_all(engine: tauri::State<'_, Engine>) -> Result<(), String> {
    engine.disconnect_all().await;
    Ok(())
}
#[tauri::command]
async fn renew(engine: tauri::State<'_, Engine>, id: String) -> Result<(), String> {
    engine.renew(&id).await
}
#[tauri::command]
async fn open_page(engine: tauri::State<'_, Engine>, id: String) -> Result<(), String> {
    engine.open(&id).await
}
#[tauri::command]
async fn update_settings(
    app: tauri::AppHandle,
    engine: tauri::State<'_, Engine>,
    settings: Settings,
) -> Result<(), String> {
    let previous = engine.settings().await;
    if settings.autostart != previous.autostart {
        if settings.autostart {
            app.autolaunch().enable()
        } else {
            app.autolaunch().disable()
        }
        .map_err(|e| e.to_string())?;
    }
    if let Err(error) = engine.save_settings(settings).await {
        let rollback = if previous.autostart {
            app.autolaunch().enable()
        } else {
            app.autolaunch().disable()
        };
        return Err(match rollback {
            Ok(()) => error,
            Err(e) => format!("{error}；恢复自启偏好失败：{e}"),
        });
    }
    Ok(())
}
#[tauri::command]
async fn diagnostics(
    app: tauri::AppHandle,
    engine: tauri::State<'_, Engine>,
) -> Result<Vec<platform::Check>, String> {
    Ok(platform::check(&app, &engine.settings().await, false).await)
}
#[tauri::command]
async fn repair(
    app: tauri::AppHandle,
    engine: tauri::State<'_, Engine>,
) -> Result<Vec<platform::Check>, String> {
    Ok(platform::check(&app, &engine.settings().await, true).await)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            let mut link = false;
            for arg in args.iter().skip(1) {
                if arg.starts_with("home-service:") || arg.starts_with("hometunnel:") {
                    incoming(app.clone(), arg.clone());
                    link = true;
                }
            }
            if !link && !args.iter().any(|a| a == "--background") {
                show(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .invoke_handler(tauri::generate_handler![
            snapshot,
            notices,
            get_settings,
            disconnect,
            disconnect_all,
            renew,
            open_page,
            update_settings,
            diagnostics,
            repair
        ])
        .setup(|app| {
            let engine = Engine::new(app.path().app_config_dir()?.join("settings.json"));
            app.manage(engine.clone());
            let window = app.get_webview_window("main").unwrap();
            platform::material(&window);
            let handle = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.hide();
                    }
                }
            });
            let open = MenuItem::with_id(app, "open", "连接管理", true, None::<&str>)?;
            let all = MenuItem::with_id(app, "all", "断开全部", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出 HomeTunnel", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &all, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("家庭隧道 · HomeTunnel")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show(app),
                    "all" => {
                        let e = app.state::<Engine>().inner().clone();
                        tauri::async_runtime::spawn(async move {
                            e.disconnect_all().await;
                        });
                    }
                    "quit" => {
                        let e = app.state::<Engine>().inner().clone();
                        let a = app.clone();
                        tauri::async_runtime::spawn(async move {
                            e.disconnect_all().await;
                            a.exit(0);
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show(tray.app_handle());
                    }
                })
                .build(app)?;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for u in event.urls() {
                    incoming(handle.clone(), u.to_string());
                }
            });
            let handle = app.handle().clone();
            let startup = engine.clone();
            tauri::async_runtime::spawn(async move {
                let repair = !std::env::args().any(|a| a == "--inspect");
                let checks = platform::check(&handle, &startup.settings().await, repair).await;
                for c in checks {
                    if !c.ok {
                        startup.notice(format!("{}：{}", c.name, c.detail)).await;
                    }
                }
                let _ = handle.emit("ready", ());
            });
            let args = std::env::args().collect::<Vec<_>>();
            let mut has_url = false;
            for arg in args.iter().skip(1) {
                if arg.starts_with("home-service:") || arg.starts_with("hometunnel:") {
                    has_url = true;
                    incoming(app.handle().clone(), arg.clone());
                }
            }
            if !has_url && !args.iter().any(|a| a == "--background") {
                show(app.handle());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("HomeTunnel could not start");
}
