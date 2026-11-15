mod reader;

use std::path::PathBuf;

use eframe::egui;
use egui_plot::{FilledArea, Line, Plot, PlotPoints, Text};
use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::{PowerMode, Pyramid, RecordBucket, find_ports};
use reader::{Control, Event, Source};

const LOGIC_BITS: usize = 8;
const LANE_PITCH: f64 = 1.5;
const DEFAULT_FOLD: u32 = 10;

fn main() -> eframe::Result {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    eframe::run_native(
        "ppk2_tool",
        eframe::NativeOptions::default(),
        Box::new(move |cc| Ok(Box::new(App::new(arg, cc.egui_ctx.clone())))),
    )
}

struct App {
    source: Source,
    trace: Pyramid<RecordBucket>,
    seconds_per_record: f64,
    visible: std::ops::RangeInclusive<f64>,
    status: String,
    dropped: u64,
    mode: PowerMode,
    voltage_mv: u16,
    power: bool,
    measuring: bool,
    follow: bool,
    auto_y: bool,
    fit_all: bool,
    window_s: f64,
}

impl App {
    fn new(arg: Option<PathBuf>, ctx: egui::Context) -> Self {
        let source = match arg {
            Some(p) if p.is_file() => reader::spawn_file(p, ctx)
                .unwrap_or_else(|e| fatal(&format!("cannot open recording: {e}"))),
            Some(p) => reader::spawn_device(p.to_string_lossy().into_owned(), DEFAULT_FOLD, ctx),
            None => {
                let port = find_ports()
                    .ok()
                    .and_then(|p| p.into_iter().next())
                    .unwrap_or_else(|| fatal("no PPK2 found; pass a port or a recording"));
                reader::spawn_device(port, DEFAULT_FOLD, ctx)
            }
        };
        App {
            seconds_per_record: source.fold as f64 / SAMPLES_PER_SECOND as f64,
            source,
            trace: Pyramid::default(),
            visible: 0.0..=1.0,
            status: "connecting".into(),
            dropped: 0,
            mode: PowerMode::Source,
            voltage_mv: 3300,
            power: false,
            measuring: false,
            follow: true,
            auto_y: true,
            fit_all: false,
            window_s: 5.0,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.source.events.try_recv() {
            match ev {
                Event::Records(r) => self.trace.extend(r),
                Event::Dropped(n) => self.dropped = n,
                Event::Status(s) => self.status = s,
            }
        }
    }

    fn send(&self, c: Control) {
        if self.source.control.send(c).is_err() {
            eprintln!("source thread is gone");
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.source.live {
                self.device_controls(ui);
                ui.separator();
            }
            ui.toggle_value(&mut self.follow, "follow");
            ui.toggle_value(&mut self.auto_y, "auto y");
            if ui.button("fit all").clicked() {
                self.fit_all = true;
                self.follow = false;
                self.auto_y = true;
            }
            ui.add(
                egui::Slider::new(&mut self.window_s, 0.01..=86_400.0)
                    .logarithmic(true)
                    .suffix(" s"),
            );
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn device_controls(&mut self, ui: &mut egui::Ui) {
        let before = self.mode;
        ui.add_enabled_ui(!self.measuring, |ui| {
            ui.selectable_value(&mut self.mode, PowerMode::Source, "source");
            ui.selectable_value(&mut self.mode, PowerMode::Ampere, "ampere");
        });
        if self.mode != before {
            self.send(Control::SetMode(self.mode));
        }
        let slider = egui::Slider::new(&mut self.voltage_mv, 800..=5000).suffix(" mV");
        if ui
            .add_enabled(self.mode == PowerMode::Source, slider)
            .drag_stopped()
        {
            self.send(Control::SetVoltage(self.voltage_mv));
        }
        if ui.toggle_value(&mut self.power, "power").changed() {
            self.send(Control::Power(self.power));
        }
        if ui.toggle_value(&mut self.measuring, "measure").changed() {
            self.send(Control::Measure(self.measuring));
        }
    }

    fn to_records(&self, seconds: f64) -> usize {
        ((seconds / self.seconds_per_record).floor().max(0.0) as usize).min(self.trace.len())
    }

    fn mid_s(&self, b: &RecordBucket) -> f64 {
        (b.start as f64 + b.len as f64 / 2.0) * self.seconds_per_record
    }

    fn plot(&mut self, ui: &mut egui::Ui) {
        let end_s = self.trace.len() as f64 * self.seconds_per_record;
        let plot = Plot::new("trace")
            .x_axis_label("s")
            .y_axis_label("µA")
            .link_axis(time_axis(), [true, false])
            .link_cursor(time_axis(), [true, false])
            .allow_scroll(false);
        let response = plot.show(ui, |p| {
            let visible_s = if self.fit_all {
                0.0..=end_s.max(self.seconds_per_record)
            } else if self.follow {
                (end_s - self.window_s).max(0.0)..=end_s.max(self.window_s)
            } else {
                let b = p.plot_bounds();
                b.min()[0]..=b.max()[0]
            };
            if self.fit_all || self.follow {
                p.set_plot_bounds_x(visible_s.clone());
            }
            self.fit_all = false;
            self.visible = visible_s.clone();
            let px = p.response().rect.width().max(1.0) as usize;
            let range = self.to_records(*visible_s.start())..self.to_records(*visible_s.end());
            let buckets = self.trace.query(range, px);
            let stats = Stats::of(&buckets);
            if self.auto_y && stats.min.is_finite() {
                p.set_plot_bounds_y(padded(stats.min, stats.max));
            }
            self.draw_current(p, &buckets);
            stats
        });
        let zoomed = response.response.hovered() && ui.input(|i| i.zoom_delta() != 1.0);
        if response.response.dragged() || zoomed {
            self.follow = false;
            self.auto_y = false;
        }
        let s = response.inner;
        ui.horizontal(|ui| {
            ui.label(format!("live {}", fmt_ua(s.last)));
            ui.separator();
            ui.label(format!("mean {}", fmt_ua(s.mean)));
            ui.label(format!("min {}", fmt_ua(s.min)));
            ui.label(format!("max {}", fmt_ua(s.max)));
            ui.separator();
            ui.label(format!(
                "{} records × {} samples, {end_s:.2} s",
                self.trace.len(),
                self.source.fold
            ));
            if self.dropped > 0 {
                ui.colored_label(egui::Color32::RED, format!("{} dropped", self.dropped));
            }
        });
    }

    fn draw_current(&self, p: &mut egui_plot::PlotUi, buckets: &[RecordBucket]) {
        let xs: Vec<f64> = buckets.iter().map(|b| self.mid_s(b)).collect();
        let mins: Vec<f64> = buckets.iter().map(|b| b.min as f64).collect();
        let maxs: Vec<f64> = buckets.iter().map(|b| b.max as f64).collect();
        let means: Vec<[f64; 2]> = xs
            .iter()
            .zip(buckets)
            .map(|(&x, b)| [x, b.mean() as f64])
            .collect();
        p.add(
            FilledArea::new("range", &xs, &mins, &maxs)
                .fill_color(egui::Color32::from_rgba_unmultiplied(80, 160, 255, 60)),
        );
        p.line(Line::new("mean", PlotPoints::new(means)).width(1.5));
    }

    fn logic_plot(&mut self, ui: &mut egui::Ui) {
        let range = self.to_records(*self.visible.start())..self.to_records(*self.visible.end());
        let px = ui.available_width().max(1.0) as usize;
        let buckets = self.trace.query(range, px);
        let label_x = *self.visible.start();
        Plot::new("logic")
            .height(LOGIC_BITS as f32 * 18.0)
            .link_axis(time_axis(), [true, false])
            .link_cursor(time_axis(), [true, false])
            .show_axes([false, false])
            .show_grid([false, false])
            .allow_zoom([true, false])
            .allow_drag([true, false])
            .allow_scroll(false)
            .include_y(-0.5)
            .include_y(LOGIC_BITS as f64 * LANE_PITCH)
            .show(ui, |p| self.draw_logic(p, &buckets, label_x));
    }

    fn draw_logic(&self, p: &mut egui_plot::PlotUi, buckets: &[RecordBucket], label_x: f64) {
        let xs: Vec<f64> = buckets.iter().map(|b| self.mid_s(b)).collect();
        for bit in 0..LOGIC_BITS {
            let base = (LOGIC_BITS - 1 - bit) as f64 * LANE_PITCH;
            let level = |mask: u8| (mask >> bit) & 1;
            let highs: Vec<f64> = buckets
                .iter()
                .map(|b| base + level(b.logic_any) as f64)
                .collect();
            let lows: Vec<f64> = buckets
                .iter()
                .map(|b| base + level(b.logic_all) as f64)
                .collect();
            let line: Vec<[f64; 2]> = xs.iter().zip(&highs).map(|(&x, &y)| [x, y]).collect();
            p.add(
                FilledArea::new(format!("D{bit} toggling"), &xs, &lows, &highs)
                    .fill_color(egui::Color32::from_rgba_unmultiplied(255, 180, 60, 90)),
            );
            p.line(Line::new(format!("D{bit}"), PlotPoints::new(line)).width(1.0));
            p.add(Text::new(
                format!("D{bit} label"),
                egui_plot::PlotPoint::new(label_x, base + 0.5),
                format!("D{bit}"),
            ));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        egui::Panel::top("controls").show(ui, |ui| self.controls(ui));
        egui::Panel::bottom("logic").show(ui, |ui| self.logic_plot(ui));
        egui::CentralPanel::default().show(ui, |ui| self.plot(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.send(Control::Shutdown);
    }
}

fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

fn time_axis() -> egui::Id {
    egui::Id::unique("ppk2_tool.time_axis")
}

fn padded(min: f32, max: f32) -> std::ops::RangeInclusive<f64> {
    let (min, max) = (min as f64, max as f64);
    let pad = ((max - min) * 0.05).max(min.abs() * 0.01).max(0.01);
    (min - pad)..=(max + pad)
}

struct Stats {
    last: f32,
    mean: f32,
    min: f32,
    max: f32,
}

impl Stats {
    fn of(buckets: &[RecordBucket]) -> Self {
        let (mut sum, mut n, mut min, mut max) = (0.0f64, 0usize, f32::INFINITY, f32::NEG_INFINITY);
        for b in buckets {
            sum += b.sum;
            n += b.len;
            min = min.min(b.min);
            max = max.max(b.max);
        }
        Stats {
            last: buckets.last().map(RecordBucket::mean).unwrap_or(f32::NAN),
            mean: if n == 0 {
                f32::NAN
            } else {
                (sum / n as f64) as f32
            },
            min,
            max,
        }
    }
}

fn fmt_ua(ua: f32) -> String {
    match ua.abs() {
        v if v.is_nan() => "–".into(),
        v if v >= 1e6 => format!("{:.3} A", ua / 1e6),
        v if v >= 1e3 => format!("{:.3} mA", ua / 1e3),
        _ => format!("{ua:.2} µA"),
    }
}
