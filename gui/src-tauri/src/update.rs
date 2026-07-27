// 系统信息辅助模块
use serde::{Deserialize, Serialize};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 获取当前应用版本
#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}

/// 获取系统信息
#[tauri::command]
pub fn get_system_info() -> SystemInfo {
    SystemInfo {
        build_type: if cfg!(debug_assertions) {
            "Debug"
        } else {
            "Release"
        }
        .to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os_version: get_os_version(),
    }
}

fn get_os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                return format!("macOS {}", version.trim());
            }
        }
        return "macOS".to_string();
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("cmd").args(["/c", "ver"]).output() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                return version.trim().to_string();
            }
        }
        return "Windows".to_string();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("uname").arg("-r").output() {
            if let Ok(version) = String::from_utf8(output.stdout) {
                return format!("Linux kernel {}", version.trim());
            }
        }
        return "Linux".to_string();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub build_type: String,
    pub platform: String,
    pub arch: String,
    pub os_version: String,
}
