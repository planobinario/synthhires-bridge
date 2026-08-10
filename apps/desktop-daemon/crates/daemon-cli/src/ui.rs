use eframe::egui;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use daemon_core::task_registry::{TaskState, TaskStatus, TaskKind};
use std::time::Duration;
use std::collections::VecDeque;

use crate::UiCmd;

#[derive(PartialEq)]
enum Tab {
    ControlPanel,
    SystemLogs,
}

pub struct BridgeApp {
    status_rx: watch::Receiver<String>,
    tasks_rx: watch::Receiver<Vec<TaskState>>,
    kill_tx: mpsc::Sender<Uuid>,
    ui_cmd_tx: mpsc::Sender<UiCmd>,
    log_rx: std::sync::mpsc::Receiver<String>,
    has_seen_bg_notice: bool,
    logs: VecDeque<String>,
    current_tab: Tab,
    show_unpair_confirm: bool,
    show_quit_confirm: bool,
    dark_mode: bool,
}

impl BridgeApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        status_rx: watch::Receiver<String>,
        tasks_rx: watch::Receiver<Vec<TaskState>>,
        kill_tx: mpsc::Sender<Uuid>,
        ui_cmd_tx: mpsc::Sender<UiCmd>,
        log_rx: std::sync::mpsc::Receiver<String>,
    ) -> Self {
        Self {
            status_rx,
            tasks_rx,
            kill_tx,
            ui_cmd_tx,
            log_rx,
            has_seen_bg_notice: false,
            logs: VecDeque::with_capacity(500),
            current_tab: Tab::ControlPanel,
            show_unpair_confirm: false,
            show_quit_confirm: false,
            dark_mode: true, // Default to dark mode
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        
        visuals.window_rounding = 8.0.into();
        visuals.widgets.noninteractive.rounding = 6.0.into();
        visuals.widgets.inactive.rounding = 6.0.into();
        visuals.widgets.hovered.rounding = 6.0.into();
        visuals.widgets.active.rounding = 6.0.into();
        
        if self.dark_mode {
            visuals.panel_fill = egui::Color32::from_rgb(18, 18, 20);
            visuals.window_fill = egui::Color32::from_rgb(25, 25, 28);
            visuals.override_text_color = Some(egui::Color32::from_rgb(230, 230, 235));
        } else {
            visuals.panel_fill = egui::Color32::from_rgb(245, 245, 250);
            visuals.window_fill = egui::Color32::from_rgb(255, 255, 255);
            visuals.override_text_color = Some(egui::Color32::from_rgb(30, 30, 35));
        }
        
        ctx.set_visuals(visuals);
    }

    fn render_task_row(&self, ui: &mut egui::Ui, task: &TaskState) {
        let text_color = if self.dark_mode { egui::Color32::from_gray(140) } else { egui::Color32::from_gray(100) };
        let title_color = if self.dark_mode { egui::Color32::WHITE } else { egui::Color32::BLACK };

        ui.horizontal(|ui| {
            // Kind icon
            let (icon, color) = match task.kind {
                TaskKind::ShellExec => ("🖧", egui::Color32::from_rgb(99, 102, 241)),
                TaskKind::FileRead => ("📖", egui::Color32::from_rgb(16, 185, 129)),
                TaskKind::FileWrite => ("✏", egui::Color32::from_rgb(245, 158, 11)),
                TaskKind::DbProxy => ("🗄", egui::Color32::from_rgb(236, 72, 153)),
                TaskKind::Other(_) => ("⚙", egui::Color32::GRAY),
            };
            ui.label(egui::RichText::new(icon).color(color).size(18.0));
            ui.add_space(8.0);

            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&task.description).strong().size(14.0).color(title_color));
                let elapsed = task.finished_at
                    .unwrap_or_else(std::time::Instant::now)
                    .duration_since(task.started_at_instant);
                ui.label(
                    egui::RichText::new(format!("Iniciado hace {:.1}s", elapsed.as_secs_f32()))
                        .small()
                        .color(text_color),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &task.status {
                    TaskStatus::Running => {
                        let btn_fill = if self.dark_mode { egui::Color32::from_rgb(40, 20, 20) } else { egui::Color32::from_rgb(255, 200, 200) };
                        let btn = egui::Button::new(egui::RichText::new("⏹ Detener").color(egui::Color32::from_rgb(255, 80, 80)))
                            .fill(btn_fill);
                        if ui.add(btn).clicked() {
                            let _ = self.kill_tx.try_send(task.id);
                        }
                        ui.label(egui::RichText::new("En proceso").color(egui::Color32::from_rgb(100, 150, 255)));
                    }
                    TaskStatus::Completed(code) => {
                        let text = match code {
                            Some(c) => format!("Completado ({})", c),
                            None => "Completado".to_string(),
                        };
                        ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(80, 200, 120)));
                    }
                    TaskStatus::Failed(err) => {
                        ui.label(egui::RichText::new(format!("Error: {}", err)).color(egui::Color32::from_rgb(255, 80, 80)));
                    }
                    TaskStatus::Killed => {
                        ui.label(egui::RichText::new("Interrumpido").color(egui::Color32::from_rgb(255, 150, 80)));
                    }
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
    }
}

impl eframe::App for BridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept native close to minimize instead
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if !self.has_seen_bg_notice {
                crate::tray::show_background_notice();
                self.has_seen_bg_notice = true;
            }
        }

        self.apply_theme(ctx);

        // Read new logs
        while let Ok(msg) = self.log_rx.try_recv() {
            if self.logs.len() >= 500 {
                self.logs.pop_front();
            }
            self.logs.push_back(msg);
        }

        ctx.request_repaint_after(Duration::from_millis(150));

        let txt_color = if self.dark_mode { egui::Color32::WHITE } else { egui::Color32::BLACK };
        let frame_bg = if self.dark_mode { egui::Color32::from_rgb(24, 24, 28) } else { egui::Color32::from_rgb(235, 235, 240) };
        let log_bg = if self.dark_mode { egui::Color32::from_rgb(10, 10, 12) } else { egui::Color32::from_rgb(220, 220, 225) };

        // Modals overlay
        if self.show_unpair_confirm {
            egui::Window::new("⚠️ Confirmar Desvinculación")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new("¿Estás seguro de que deseas desvincular este PC de SynthHires?").color(txt_color));
                    ui.label(egui::RichText::new("Esto eliminará la credencial de forma segura y cerrará la conexión actual.").small().color(egui::Color32::GRAY));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.show_unpair_confirm = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = egui::Button::new(egui::RichText::new("Desvincular").color(egui::Color32::WHITE))
                                .fill(egui::Color32::from_rgb(200, 50, 50));
                            if ui.add(btn).clicked() {
                                let _ = self.ui_cmd_tx.try_send(UiCmd::Unpair);
                                self.show_unpair_confirm = false;
                            }
                        });
                    });
                });
        }

        if self.show_quit_confirm {
            egui::Window::new("🛑 Confirmar Salida")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new("¿Estás seguro de que deseas cerrar completamente el Bridge?").color(txt_color));
                    ui.label(egui::RichText::new("El agente perderá acceso al sistema y las tareas en proceso podrían fallar.").small().color(egui::Color32::GRAY));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancelar").clicked() {
                            self.show_quit_confirm = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = egui::Button::new(egui::RichText::new("Cerrar Bridge").color(egui::Color32::WHITE))
                                .fill(egui::Color32::from_rgb(200, 50, 50));
                            if ui.add(btn).clicked() {
                                std::process::exit(0);
                            }
                        });
                    });
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Header
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.heading(egui::RichText::new("SynthHires Bridge").strong().size(22.0).color(txt_color));
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(egui::Color32::GRAY));
                });
                
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status = self.status_rx.borrow().clone();
                    let color = if status.contains("Conectado") {
                        egui::Color32::from_rgb(50, 200, 100)
                    } else if status.contains("Esperando") {
                        egui::Color32::from_gray(150)
                    } else {
                        egui::Color32::from_rgb(220, 150, 50)
                    };
                    
                    egui::Frame::none()
                        .fill(if self.dark_mode { egui::Color32::from_black_alpha(50) } else { egui::Color32::from_black_alpha(15) })
                        .rounding(15.0)
                        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(format!("● {}", status)).color(color).strong());
                        });
                        
                    ui.add_space(10.0);
                    
                    // Dark / Light Toggle
                    let icon = if self.dark_mode { "☀️" } else { "🌙" };
                    if ui.button(egui::RichText::new(icon).size(18.0)).clicked() {
                        self.dark_mode = !self.dark_mode;
                    }
                });
            });
            
            ui.add_space(15.0);
            
            // Tabs
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.current_tab, Tab::ControlPanel, "🎛 Panel de Control");
                ui.selectable_value(&mut self.current_tab, Tab::SystemLogs, "📜 Registro del Sistema");
            });
            ui.separator();
            ui.add_space(10.0);
            
            match self.current_tab {
                Tab::ControlPanel => {
                    ui.label(egui::RichText::new("Subprocesos Activos").strong().size(16.0).color(txt_color));
                    ui.add_space(8.0);
                    
                    let tasks = self.tasks_rx.borrow().clone();
                    
                    egui::Frame::none()
                        .fill(frame_bg)
                        .rounding(6.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false; 2])
                                .max_height(340.0)
                                .show(ui, |ui| {
                                    if tasks.is_empty() {
                                        ui.vertical_centered(|ui| {
                                            ui.add_space(40.0);
                                            ui.label(egui::RichText::new("El agente está inactivo.\nNo hay tareas en ejecución.").color(egui::Color32::GRAY));
                                        });
                                    } else {
                                        for task in &tasks {
                                            self.render_task_row(ui, task);
                                        }
                                    }
                                });
                        });
                }
                Tab::SystemLogs => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Transparencia en tiempo real").strong().size(16.0).color(txt_color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("🧹 Limpiar").clicked() {
                                self.logs.clear();
                            }
                            if ui.button("📋 Copiar").clicked() {
                                let all_logs = self.logs.iter().cloned().collect::<Vec<_>>().join("\n");
                                ui.output_mut(|o| o.copied_text = all_logs);
                            }
                        });
                    });
                    ui.add_space(8.0);
                    
                    egui::Frame::none()
                        .fill(log_bg)
                        .rounding(4.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false; 2])
                                .stick_to_bottom(true)
                                .max_height(340.0)
                                .show(ui, |ui| {
                                    for log in &self.logs {
                                        let log_color = if self.dark_mode { egui::Color32::from_gray(180) } else { egui::Color32::from_gray(60) };
                                        ui.label(egui::RichText::new(log).family(egui::FontFamily::Monospace).size(12.0).color(log_color));
                                    }
                                });
                        });
                }
            }
            
            // Bottom Actions
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("🌐 Abrir Dashboard").clicked() {
                        let _ = self.ui_cmd_tx.try_send(UiCmd::OpenDashboard);
                    }
                    if ui.button("🔽 Minimizar").clicked() {
                        if !self.has_seen_bg_notice {
                            crate::tray::show_background_notice();
                            self.has_seen_bg_notice = true;
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                    
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let quit_btn = egui::Button::new(egui::RichText::new("Salir").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(150, 40, 40));
                        if ui.add(quit_btn).clicked() {
                            self.show_quit_confirm = true;
                        }
                        
                        let is_waiting = self.status_rx.borrow().contains("Esperando emparejamiento");
                        if is_waiting {
                            let pair_fill = if self.dark_mode { egui::Color32::from_rgb(20, 50, 20) } else { egui::Color32::from_rgb(220, 255, 220) };
                            let pair_btn = egui::Button::new(egui::RichText::new("🔗 Vincular con SynthHires").color(egui::Color32::from_rgb(50, 180, 50)).strong())
                                .fill(pair_fill);
                            if ui.add(pair_btn).clicked() {
                                let _ = self.ui_cmd_tx.try_send(UiCmd::OpenDashboard);
                            }
                        } else {
                            let unpair_fill = if self.dark_mode { egui::Color32::from_rgb(50, 25, 25) } else { egui::Color32::from_rgb(255, 230, 230) };
                            let unpair_btn = egui::Button::new(egui::RichText::new("Desvincular Dispositivo").color(egui::Color32::from_rgb(220, 80, 80)))
                                .fill(unpair_fill);
                            if ui.add(unpair_btn).clicked() {
                                self.show_unpair_confirm = true;
                            }
                        }
                    });
                });
                ui.add_space(10.0);
                ui.separator();
            });
        });
    }
}
