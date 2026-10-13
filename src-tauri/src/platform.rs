use crate::engine::{self, Settings};
use serde::Serialize;
#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_deep_link::DeepLinkExt;
#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
fn item(name: &str, result: Result<String, String>) -> Check {
    match result {
        Ok(detail) => Check {
            name: name.into(),
            ok: true,
            detail,
        },
        Err(detail) => Check {
            name: name.into(),
            ok: false,
            detail,
        },
    }
}

pub async fn check(app: &tauri::AppHandle, settings: &Settings, repair: bool) -> Vec<Check> {
    let mut checks = vec![];
    #[cfg(windows)]
    {
        let mut result = Ok("协议指向当前安装".into());
        for scheme in ["home-service"] {
            let valid = registered_to_self(scheme);
            if !valid && repair {
                if let Err(e) = app.deep_link().register(scheme) {
                    result = Err(e.to_string());
                }
            }
            if !registered_to_self(scheme) {
                result = Err("协议未指向当前安装，点击修复或重新运行安装器".into());
            }
        }
        if repair {
            association_changed();
        }
        checks.push(item("应用协议", result));
    }
    #[cfg(target_os = "macos")]
    {
        let result = mac_registration(app, repair).await;
        checks.push(item("应用协议", result));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    checks.push(item("应用协议", Ok("此构建未作为正式发行平台".into())));
    let actual = app.autolaunch().is_enabled().map_err(|e| e.to_string());
    if matches!(actual, Ok(true)) && settings.autostart && !startup_points_here(app) {
        let result = if repair {
            app.autolaunch()
                .enable()
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    if startup_points_here(app) {
                        Ok("已修复为当前安装路径".into())
                    } else {
                        Err("自启路径修复后仍不一致".into())
                    }
                })
        } else {
            Err("自启仍指向其他安装路径".into())
        };
        checks.push(item("登录自启", result));
    } else {
        let result = match actual {
            Ok(value) if value == settings.autostart => Ok(if value {
                "已按设置启用"
            } else {
                "已按设置关闭"
            }
            .into()),
            Ok(_) if repair => {
                let r = if settings.autostart {
                    app.autolaunch().enable()
                } else {
                    app.autolaunch().disable()
                };
                r.map(|_| "已修复为保存的偏好".into())
                    .map_err(|e| e.to_string())
            }
            Ok(_) => Err("与保存偏好不一致".into()),
            Err(e) => Err(e),
        };
        checks.push(item("登录自启", result));
    }
    let ssh = engine::command("ssh").arg("-V").output().await;
    checks.push(item(
        "OpenSSH",
        ssh.map_err(|_| "找不到系统OpenSSH客户端".into())
            .and_then(|o| {
                if o.status.success() {
                    Ok("系统客户端可用".into())
                } else {
                    Err("系统客户端无法运行".into())
                }
            }),
    ));
    checks.push(item(
        "SSH Agent",
        engine::agent_available()
            .await
            .map(|_| "已发现可用公钥身份；签名仍由Agent确认".into()),
    ));
    checks
}
#[cfg(windows)]
fn registered_to_self(scheme: &str) -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let current = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return false,
    };
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!("Software\\Classes\\{scheme}"));
    let Ok(key) = key else { return false };
    if key.get_value::<String, _>("URL Protocol").is_err() {
        return false;
    }
    key.open_subkey("shell\\open\\command")
        .and_then(|k| k.get_value::<String, _>(""))
        .map(|v| v.eq_ignore_ascii_case(&format!("\"{current}\" \"%1\"")))
        .unwrap_or(false)
}
#[cfg(windows)]
fn association_changed() {
    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            a: *const std::ffi::c_void,
            b: *const std::ffi::c_void,
        );
    }
    unsafe {
        SHChangeNotify(0x08000000, 0x2000, std::ptr::null(), std::ptr::null());
    }
}

#[cfg(target_os = "macos")]
async fn mac_registration(app: &tauri::AppHandle, repair: bool) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("应用Bundle路径无效")?;
    if bundle.extension().and_then(|s| s.to_str()) != Some("app") {
        return Err("请将正式应用安装到Applications后运行".into());
    }
    if repair {
        let status=engine::command("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister").arg("-f").arg(bundle).status().await.map_err(|e|e.to_string())?;
        if !status.success() {
            return Err("Launch Services注册失败".into());
        }
    }
    mac_default_handlers(&app.config().identifier, repair)?;
    Ok("协议已指向当前安装".into())
}
pub fn material(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        use tauri::window::{Effect, EffectsBuilder};
        let _ = window.set_effects(EffectsBuilder::new().effect(Effect::Mica).build());
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
        use objc2::{
            class, msg_send,
            runtime::{AnyClass, AnyObject},
        };
        use objc2_foundation::NSRect;
        // NSGlassEffectView is runtime-checked; older systems use native vibrancy.
        if let Ok(ptr) = window.ns_window() {
            let win = &*(ptr as *const AnyObject);
            let _: () = msg_send![win,setTitleVisibility:1isize];
            let content: *mut AnyObject = msg_send![win, contentView];
            let bounds: NSRect = msg_send![&*content, bounds];
            let glass_class = AnyClass::get(c"NSGlassEffectView");
            let cls = glass_class.unwrap_or_else(|| class!(NSVisualEffectView));
            let allocated: *mut AnyObject = msg_send![cls, alloc];
            let glass: *mut AnyObject = msg_send![allocated,initWithFrame:bounds];
            let _: () = msg_send![&*glass,setAutoresizingMask:18usize];
            if glass_class.is_none() {
                let _: () = msg_send![&*glass,setBlendingMode:0isize];
                let _: () = msg_send![&*glass,setMaterial:12isize];
                let _: () = msg_send![&*glass,setState:1isize];
            }
            if glass_class.is_some() {
                // Host the real content in the glass; a separate background sibling does not integrate it.
                let _: () = msg_send![&*content, retain];
                let _: () = msg_send![&*glass,setCornerRadius:18.0f64];
                let _: () = msg_send![&*glass,setContentView:content];
                let _: () = msg_send![win,setContentView:glass];
                let _: () = msg_send![&*content, release];
            } else {
                let _: () = msg_send![&*content,addSubview:glass positioned:-1isize relativeTo:std::ptr::null::<AnyObject>()];
            }
            let _: () = msg_send![win,setOpaque:false];
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![win,setBackgroundColor:clear];
            let _: () = msg_send![&*glass, release];
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = window;
}

pub fn replace_file(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let source = from
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let target = to
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                9,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to).map_err(|e| e.to_string())
    }
}

// The kernel closes this handle even if the manager crashes: owned SSH children die with it.
#[cfg(windows)]
pub struct OwnedJob(usize);
#[cfg(windows)]
impl OwnedJob {
    pub fn attach(pid: u32) -> Result<Self, String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::{JobObjects::*, Threading::*},
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let guard = Self(job as usize);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let assigned = AssignProcessToJobObject(job, process);
            let error = std::io::Error::last_os_error();
            CloseHandle(process);
            if assigned == 0 {
                return Err(error.to_string());
            }
            Ok(guard)
        }
    }
}
#[cfg(windows)]
impl Drop for OwnedJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as *mut _);
        }
    }
}

#[cfg(target_os = "macos")]
fn mac_default_handlers(identifier: &str, repair: bool) -> Result<(), String> {
    use std::ffi::{c_char, c_void, CString};
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(a: *const c_void, s: *const c_char, e: u32) -> *const c_void;
        fn CFEqual(a: *const c_void, b: *const c_void) -> u8;
        fn CFRelease(a: *const c_void);
    }
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSCopyDefaultHandlerForURLScheme(s: *const c_void) -> *const c_void;
        fn LSSetDefaultHandlerForURLScheme(s: *const c_void, b: *const c_void) -> i32;
    }
    let bundle = CString::new(identifier).map_err(|e| e.to_string())?;
    unsafe {
        let expected = CFStringCreateWithCString(std::ptr::null(), bundle.as_ptr(), 0x08000100);
        if expected.is_null() {
            return Err("无法读取应用标识".into());
        }
        let mut ok = true;
        for name in [c"home-service"] {
            let scheme = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), 0x08000100);
            if scheme.is_null() {
                ok = false;
                continue;
            }
            if repair && LSSetDefaultHandlerForURLScheme(scheme, expected) != 0 {
                ok = false;
            }
            let actual = LSCopyDefaultHandlerForURLScheme(scheme);
            if actual.is_null() {
                ok = false;
            } else {
                if CFEqual(actual, expected) == 0 {
                    ok = false;
                }
                CFRelease(actual);
            }
            CFRelease(scheme);
        }
        CFRelease(expected);
        if ok {
            Ok(())
        } else {
            Err("默认协议处理程序未指向此应用，请重新安装或修复登记".into())
        }
    }
}

fn startup_points_here(app: &tauri::AppHandle) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        let value = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
            .and_then(|key| key.get_value::<String, _>(&app.package_info().name));
        let path = exe.to_string_lossy();
        value
            .map(|value| {
                value.eq_ignore_ascii_case(&format!("\"{path}\" --background"))
                    || value.eq_ignore_ascii_case(&format!("{path} --background"))
            })
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        let text = std::fs::read_to_string(
            home.join("Library/LaunchAgents")
                .join(format!("{}.plist", app.package_info().name)),
        )
        .unwrap_or_default();
        let path = exe
            .to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        text.contains(&path) && text.contains("--background")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (app, exe);
        true
    }
}

pub fn tray_image<'a>(default: tauri::image::Image<'a>) -> tauri::image::Image<'a> {
    #[cfg(target_os = "macos")]
    {
        let size = 44usize;
        let mut rgba = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let mut coverage = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = (x as f64 + (sx as f64 + 0.5) / 4.0) / 2.0 - 11.0;
                        let py = (y as f64 + (sy as f64 + 0.5) / 4.0) / 2.0 - 11.0;
                        let u = (px - py) * std::f64::consts::FRAC_1_SQRT_2;
                        let v = (px + py) * std::f64::consts::FRAC_1_SQRT_2;
                        let outline = [-3.0, 3.0].iter().any(|center| {
                            let dx = (u - center).abs() - 2.5;
                            let distance = (dx.max(0.0).powi(2) + v.powi(2)).sqrt();
                            (distance - 3.0).abs() < 0.85
                        });
                        if outline {
                            coverage += 1;
                        }
                    }
                }
                rgba[(y * size + x) * 4 + 3] = (coverage * 255 / 16) as u8;
            }
        }
        let _ = default;
        tauri::image::Image::new_owned(rgba, size as u32, size as u32)
    }
    #[cfg(not(target_os = "macos"))]
    {
        default
    }
}
