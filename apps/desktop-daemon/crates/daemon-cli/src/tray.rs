use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};

use daemon_core::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};

use super::DaemonState;

pub struct TrayHandle {
    _tray: tray_icon::TrayIcon,
}

pub fn build_tray(
    _state: Arc<RwLock<DaemonState>>,
    _config_dir: std::path::PathBuf,
    _port: u16,
) -> Result<(TrayHandle, tokio::sync::oneshot::Receiver<()>)> {
    let (quit_tx, quit_rx) = oneshot::channel();
    let icon = load_icon();

    let menu = Menu::new();

    let status: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🟢 SynthHires Bridge Daemon — Activo", false, None)));
    menu.append(status).ok();

    let sep: &'static PredefinedMenuItem = Box::leak(Box::new(PredefinedMenuItem::separator()));
    menu.append(sep).ok();

    let disconnect_item: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🔌 Desconectar", true, None)));
    let disconnect_id = disconnect_item.id();
    menu.append(disconnect_item).ok();

    let sep2: &'static PredefinedMenuItem = Box::leak(Box::new(PredefinedMenuItem::separator()));
    menu.append(sep2).ok();

    let quit_item: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🚪 Salir del Bridge", true, None)));
    let quit_id = quit_item.id();
    menu.append(quit_item).ok();

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("SynthHires Bridge — 🟢 Activo (127.0.0.1)")
        .build()
        .map_err(|e| {
            daemon_core::DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("tray: {e}"),
            ))
        })?;

    std::thread::spawn(move || {
        loop {
            let Ok(event) = MenuEvent::receiver().recv() else {
                break;
            };

            if event.id == quit_id {
                let _ = quit_tx.send(());
                std::process::exit(0);
            }

            if event.id == disconnect_id {
                tracing::info!("Disconnect requested from tray.");
            }
        }
    });

    let handle = TrayHandle { _tray: tray };
    Ok((handle, quit_rx))
}

const TRAY_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

fn load_icon() -> Icon {
    let img = image::load_from_memory(TRAY_ICON_PNG)
        .expect("failed to decode embedded tray icon")
        .into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h)
        .expect("failed to create tray icon from RGBA")
}
