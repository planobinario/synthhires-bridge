use eframe::egui;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use daemon_core::task_registry::{TaskState, TaskStatus, TaskKind};
use std::time::Duration;

pub struct BridgeApp {
    status_rx: watch::Receiver<String>,
    tasks_rx: watch::Receiver<Vec<TaskState>>,
    kill_tx: mpsc::Sender<Uuid>,
    has_seen_bg_notice: bool,
    is_already_running: bool,
}

impl BridgeApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        status_rx: watch::Receiver<String>,
        tasks_rx: watch::Receiver<Vec<TaskState>>,
        kill_tx: mpsc::Sender<Uuid>,
        is_already_running: bool,
    ) -> Self {
        Self {
            status_rx,
            tasks_rx,
            kill_tx,
            has_seen_bg_notice: false,
            is_already_running,
        }
    }

    fn render_task_row(&self, ui: &mut egui::Ui, task: &TaskState) {
        ui.horizontal(|ui| {
            // Kind icon
            let (icon, color) = match task.kind {
                TaskKind::ShellExec => ("­ƒûº", egui::Color32::from_rgb(99, 102, 241)), // Indigo
                TaskKind::FileRead => ("­ƒôû", egui::Color32::from_rgb(16, 185, 129)), // Emerald
                TaskKind::FileWrite => ("Ô£Å", egui::Color32::from_rgb(245, 158, 11)), // Amber
                TaskKind::DbProxy => ("­ƒùä", egui::Color32::from_rgb(236, 72, 153)), // Pink
                TaskKind::Other(_) => ("ÔÜÖ", egui::Color32::GRAY),
            };
            ui.label(egui::RichText::new(icon).color(color).size(16.0));

            // Description and timing
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&task.description).strong());
                let elapsed = task.finished_at
                    .unwrap_or_else(std::time::Instant::now)
                    .duration_since(task.started_at_instant);
                ui.label(
                    egui::RichText::new(format!("Iniciado hace {:.1}s", elapsed.as_secs_f32()))
                        .small()
                        .color(egui::Color32::DARK_GRAY),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Status badge & Action
                match &task.status {
                    TaskStatus::Running => {
                        if ui.button(egui::RichText::new("ÔÅ╣ Detener").color(egui::Color32::RED)).clicked() {
                            let _ = self.kill_tx.try_send(task.id);
                        }
                        ui.label(egui::RichText::new("En proceso").color(egui::Color32::LIGHT_BLUE));
                    }
                    TaskStatus::Completed(code) => {
                        let text = match code {
                            Some(c) => format!("Completado ({})", c),
                            None => "Completado".to_string(),
                        };
                        ui.label(egui::RichText::new(text).color(egui::Color32::GREEN));
                    }
                    TaskStatus::Failed(err) => {
                        ui.label(egui::RichText::new(format!("Error: {}", err)).color(egui::Color32::RED));
                    }
                    TaskStatus::Killed => {
                        ui.label(egui::RichText::new("Interrumpido").color(egui::Color32::ORANGE));
                    }
                }
            });
        });
        ui.separator();
    }
}

impl eframe::App for BridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Continuous UI updates when status changes
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.is_already_running {
                ui.vertical_centered(|ui| {
                    ui.add_space(50.0);
                    ui.label(egui::RichText::new("ÔÜá Error").color(egui::Color32::RED).size(32.0).strong());
                    ui.add_space(20.0);
                    ui.label(egui::RichText::new("El Daemon ya se est├í ejecutando en segundo plano.").size(16.0));
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Para abrir una nueva instancia, primero debes cerrar la anterior haciendo clic derecho en el icono de SynthHires en la bandeja del sistema (junto al reloj) y seleccionando 'Salir del Bridge'.").color(egui::Color32::DARK_GRAY));
                    ui.add_space(30.0);
                    if ui.button(egui::RichText::new("Salir").size(16.0)).clicked() {
                        std::process::exit(0);
                    }
                });
                return;
            }

            // Header
            ui.horizontal(|ui| {
                ui.heading("SynthHires Bridge");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status = self.status_rx.borrow().clone();
                    let color = if status.contains("Conectado") {
                        egui::Color32::GREEN
                    } else if status.contains("Reconectando") {
                        egui::Color32::GOLD
                    } else {
                        egui::Color32::RED
                    };
                    ui.label(egui::RichText::new(format!("ÔùÅ {}", status)).color(color).strong());
                });
            });
            
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(5.0);
            
            // Task List
            ui.label(egui::RichText::new("Subprocesos Activos").strong().size(14.0));
            ui.add_space(5.0);
            
            let tasks = self.tasks_rx.borrow().clone();
            
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .max_height(350.0)
                .show(ui, |ui| {
                    if tasks.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(20.0);
                            ui.label(egui::RichText::new("No hay tareas en ejecuci├│n").color(egui::Color32::DARK_GRAY));
                        });
                    } else {
                        for task in &tasks {
                            self.render_task_row(ui, task);
                        }
                    }
                });
            
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cerrar ventana").clicked() {
                        if !self.has_seen_bg_notice {
                            crate::tray::show_background_notice();
                            self.has_seen_bg_notice = true;
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                    if ui.button("Desconectar").clicked() {
                        // TODO: Implement soft disconnect
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Salir del Bridge").color(egui::Color32::RED)).clicked() {
                            std::process::exit(0);
                        }
                    });
                });
                ui.add_space(5.0);
                ui.label(egui::RichText::new("Cerrar esta ventana no detiene el Bridge. Usa 'Salir del Bridge' para cerrarlo por completo.").small().color(egui::Color32::DARK_GRAY));
                ui.separator();
            });
        });
    }
}
