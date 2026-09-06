//! 诊断基线：最小 eframe 应用，空 UI（测 egui+glow 驱动的内存底噪）。
use eframe::egui;

struct App;
impl eframe::App for App {
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}
}

fn main() -> eframe::Result {
    eframe::run_native(
        "mss-baseline",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_decorations(false)
                .with_resizable(false)
                .with_active(false)
                .with_taskbar(false)
                .with_inner_size([1.0, 1.0]),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(App))),
    )
}
