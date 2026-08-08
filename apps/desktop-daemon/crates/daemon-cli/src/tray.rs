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
    ui_ctx: Arc<RwLock<Option<eframe::egui::Context>>>,
    ui_cmd_tx: tokio::sync::mpsc::Sender<crate::UiCmd>,
) -> Result<(TrayHandle, tokio::sync::oneshot::Receiver<()>)> {
    let (quit_tx, quit_rx) = oneshot::channel();
    let icon = load_icon();

    let menu = Menu::new();

    let status: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🟢 SynthHires Bridge Daemon — Activo", false, None)));
    menu.append(status).ok();

    let sep: &'static PredefinedMenuItem = Box::leak(Box::new(PredefinedMenuItem::separator()));
    menu.append(sep).ok();

    let show_window_item: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🖼 Mostrar Ventana", true, None)));
    let show_window_id = show_window_item.id();
    menu.append(show_window_item).ok();

    let disconnect_item: &'static MenuItem =
        Box::leak(Box::new(MenuItem::new("🔌 Desvincular Dispositivo", true, None)));
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

            if event.id == show_window_id {
                let rt = tokio::runtime::Handle::try_current();
                if let Ok(rt) = rt {
                    rt.block_on(async {
                        let ctx = ui_ctx.read().await;
                        if let Some(ctx) = ctx.as_ref() {
                            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
                            // Also ensure it's not minimized
                            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::InnerSize(eframe::egui::vec2(400.0, 500.0)));
                            tracing::info!("Focus command sent to UI");
                        }
                    });
                }
            }

            if event.id == disconnect_id {
                let rt = tokio::runtime::Handle::try_current();
                if let Ok(rt) = rt {
                    rt.block_on(async {
                        let _ = ui_cmd_tx.send(crate::UiCmd::Unpair).await;
                    });
                }
            }
        }
    });

    let handle = TrayHandle { _tray: tray };
    Ok((handle, quit_rx))
}

pub fn show_background_notice() {
    use notify_rust::Notification;
    let _ = Notification::new()
        .summary("SynthHires Bridge")
        .body("Sigue activo en segundo plano. Haz clic en el icono de la bandeja para verlo o cerrarlo.")
        .icon("dialog-information")
        .show();
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

fn fallback_icon() -> Icon {
    let w = 32u32;
    let h = 32u32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx] = 248;
            pixels[idx + 1] = 248;
            pixels[idx + 2] = 252;
            pixels[idx + 3] = 255;
        }
    }

    let cx = (w / 2) as i32;
    let cy = (h / 2) as i32;

    draw_line(&mut pixels, w, h, cx - 6, cy - 5, cx - 1, cy, 100, 116, 139, 3);
    draw_line(&mut pixels, w, h, cx - 1, cy, cx - 6, cy + 5, 100, 116, 139, 3);
    draw_line(&mut pixels, w, h, cx, cy - 5, cx + 5, cy, 99, 102, 241, 3);
    draw_line(&mut pixels, w, h, cx + 5, cy, cx, cy + 5, 99, 102, 241, 3);

    Icon::from_rgba(pixels, w, h).expect("failed to create fallback tray icon")
}

fn draw_line(
    pixels: &mut [u8], w: u32, h: u32,
    x1: i32, y1: i32, x2: i32, y2: i32,
    r: u8, g: u8, b: u8, thickness: i32,
) {
    let dx = (x2 - x1).abs();
    let dy = -(y2 - y1).abs();
    let sx = if x1 < x2 { 1 } else { -1 };
    let sy = if y1 < y2 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x1;
    let mut y = y1;

    loop {
        for tx in -thickness..=thickness {
            for ty in -thickness..=thickness {
                if tx * tx + ty * ty > thickness * thickness { continue; }
                let px = x + tx;
                let py = y + ty;
                if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                    let idx = ((py as u32 * w + px as u32) * 4) as usize;
                    pixels[idx] = r;
                    pixels[idx + 1] = g;
                    pixels[idx + 2] = b;
                    pixels[idx + 3] = 255;
                }
            }
        }
        if x == x2 && y == y2 { break; }
        let e2 = 2 * err;
        if e2 >= dy { if x == x2 { break; } err += dy; x += sx; }
        if e2 <= dx { if y == y2 { break; } err += dx; y += sy; }
    }
}
