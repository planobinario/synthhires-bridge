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
}

impl BridgeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        status_rx: watch::Receiver<String>,
        tasks_rx: watch::Receiver<Vec<TaskState>>,
        kill_tx: mpsc::Sender<Uuid>,
        ui_cmd_tx: mpsc::Sender<UiCmd>,
        log_rx: std::sync::mpsc::Receiver<String>,
    ) -> Self {
        // Setup premium dark theme
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = 8.0.into();
        visuals.widgets.noninteractive.rounding = 6.0.into();
        visuals.widgets.inactive.rounding = 6.0.into();
        visuals.widgets.hovered.rounding = 6.0.into();
        visuals.widgets.active.rounding = 6.0.into();
        
        visuals.panel_fill = egui::Color32::from_rgb(18, 18, 20); // Very dark gray/blue
        visuals.window_fill = egui::Color32::from_rgb(25, 25, 28);
        visuals.override_text_color = Some(egui::Color32::from_rgb(230, 230, 235));
        
        cc.egui_ctx.set_visuals(visuals);

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
        }
    }

    fn render_task_row(&self, ui: &mut egui::Ui, task: &TaskState) {
        ui.horizontal(|ui| {
            // Kind icon
            let (icon, color) = match task.kind {
                TaskKind::ShellExec => ("🖧", egui::Color32::from_rgb(99, 102, 241)), // Indigo
                TaskKind::FileRead => ("📖", egui::Color32::from_rgb(16, 185, 129)), // Emerald
                TaskKind::FileWrite => ("✏", egui::Color32::from_rgb(245, 158, 11)), // Amber
                TaskKind::DbProxy => ("🗄", egui::Color32::from_rgb(236, 72, 153)), // Pink
                TaskKind::Other(_) => ("⚙", egui::Color32::GRAY),
            };
            ui.label(egui::RichText::new(icon).color(color).size(18.0));
            ui.add_space(8.0);

            // Description and timing
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&task.description).strong().size(14.0));
                let elapsed = task.finished_at
                    .unwrap_or_else(std::time::Instant::now)
                    .duration_since(task.started_at_instant);
                ui.label(
                    egui::RichText::new(format!("Iniciado hace {:.1}s", elapsed.as_secs_f32()))
                        .small()
                        .color(egui::Color32::from_gray(140)),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Status badge & Action
                match &task.status {
                    TaskStatus::Running => {
                        let btn = egui::Button::new(egui::RichText::new("⏹ Detener").color(egui::Color32::from_rgb(255, 80, 80)))
                            .fill(egui::Color32::from_rgb(40, 20, 20));
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
                        ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(80, 220, 120)));
                    }
                    TaskStatus::Failed(err) => {
                        ui.label(egui::RichText::new(format!("Error: {}", err)).color(egui::Color32::from_rgb(255, 100, 100)));
                    }
                    TaskStatus::Killed => {
                        ui.label(egui::RichText::new("Interrumpido").color(egui::Color32::from_rgb(255, 180, 80)));
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
        // Read new logs
        while let Ok(msg) = self.log_rx.try_recv() {
            if self.logs.len() >= 500 {
                self.logs.pop_front();
            }
            self.logs.push_back(msg);
        }

        // Continuous UI updates when status changes
        ctx.request_repaint_after(Duration::from_millis(150));

        // Modals overlay
        if self.show_unpair_confirm {
            egui::Window::new("⚠️ Confirmar Desvinculación")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("¿Estás seguro de que deseas desvincular este PC de SynthHires?");
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
                    ui.label("¿Estás seguro de que deseas cerrar completamente el Bridge?");
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
                    ui.heading(egui::RichText::new("SynthHires Bridge").strong().size(22.0));
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(egui::Color32::from_gray(120)));
                });
                
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status = self.status_rx.borrow().clone();
                    let color = if status.contains("Conectado") {
                        egui::Color32::from_rgb(80, 220, 120)
                    } else if status.contains("Esperando") {
                        egui::Color32::from_rgb(180, 180, 180)
                    } else {
                        egui::Color32::from_rgb(240, 180, 80)
                    };
                    
                    egui::Frame::none()
                        .fill(egui::Color32::from_black_alpha(50))
                        .rounding(15.0)
                        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(format!("● {}", status)).color(color).strong());
                        });
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
                    // Task List
                    ui.label(egui::RichText::new("Subprocesos Activos").strong().size(16.0).color(egui::Color32::from_rgb(200, 200, 210)));
                    ui.add_space(8.0);
                    
                    let tasks = self.tasks_rx.borrow().clone();
                    
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(24, 24, 28))
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
                                            ui.label(egui::RichText::new("El agente está inactivo.\nNo hay tareas en ejecución.").color(egui::Color32::from_gray(100)));
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
                    ui.label(egui::RichText::new("Transparencia en tiempo real").strong().size(16.0).color(egui::Color32::from_rgb(200, 200, 210)));
                    ui.add_space(8.0);
                    
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(10, 10, 12))
                        .rounding(4.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false; 2])
                                .stick_to_bottom(true)
                                .max_height(340.0)
                                .show(ui, |ui| {
                                    for log in &self.logs {
                                        ui.label(egui::RichText::new(log).family(egui::FontFamily::Monospace).size(12.0).color(egui::Color32::from_gray(180)));
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
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close); // Note: eframe's close hides it when run_native handles it or if it intercepts close. 
                    }
                    
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let quit_btn = egui::Button::new(egui::RichText::new("Salir").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(150, 40, 40));
                        if ui.add(quit_btn).clicked() {
                            self.show_quit_confirm = true;
                        }
                        
                        let unpair_btn = egui::Button::new(egui::RichText::new("Desvincular Dispositivo").color(egui::Color32::from_rgb(255, 100, 100)))
                            .fill(egui::Color32::from_rgb(50, 25, 25));
                        if ui.add(unpair_btn).clicked() {
                            self.show_unpair_confirm = true;
                        }
                    });
                });
                ui.add_space(10.0);
                ui.separator();
            });
        });
    }
}
