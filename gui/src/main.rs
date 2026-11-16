mod client;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use client::{Event, Session};
use eframe::egui;
use egui_plot::{Axis, FilledArea, Line, Plot, PlotPoints, Span, Text};
use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::proto::Request;
use ppk2_tool::settings::socket_path;
use ppk2_tool::{PowerMode, Pyramid, RecordBucket};

const LOGIC_BITS: usize = 8;
const LANE_PITCH: f64 = 1.5;
const STATUS_EVERY: Duration = Duration::from_millis(500);

fn main() -> eframe::Result {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    eframe::run_native(
        "ppk2_tool",
        eframe::NativeOptions::default(),
        Box::new(move |cc| Ok(Box::new(App::new(arg, cc.egui_ctx.clone())))),
    )
}

struct App {
    session: Session,
    trace: Pyramid<RecordBucket>,
    seconds_per_record: f64,
    visible: std::ops::RangeInclusive<f64>,
    status: String,
    dropped: u64,
    mode: PowerMode,
    voltage_mv: u16,
    fold: u32,
    power: bool,
    measuring: bool,
    keep_daemon: bool,
    daemon_warning: Option<String>,
    save_path: String,
    last_status: Instant,
    follow: bool,
    auto_y: bool,
    fit_all: bool,
    window_s: f64,
    selection: Option<std::ops::RangeInclusive<f64>>,
    hover_s: Option<f64>,
    view: Stats,
}

impl App {
    fn new(arg: Option<PathBuf>, ctx: egui::Context) -> Self {
        let session = match arg {
            Some(p) => Session::file(p, ctx)
                .unwrap_or_else(|e| fatal(&format!("cannot open recording: {e}"))),
            None => Session::daemon(&socket_path(), ctx)
                .unwrap_or_else(|e| fatal(&format!("no daemon and could not start one: {e}"))),
        };
        let mut app = App {
            seconds_per_record: session.fold as f64 / SAMPLES_PER_SECOND as f64,
            fold: session.fold,
            session,
            trace: Pyramid::default(),
            visible: 0.0..=1.0,
            status: "connecting".into(),
            dropped: 0,
            mode: PowerMode::Source,
            voltage_mv: 3300,
            power: false,
            measuring: false,
            keep_daemon: false,
            daemon_warning: None,
            save_path: String::new(),
            last_status: Instant::now() - STATUS_EVERY,
            follow: true,
            auto_y: true,
            fit_all: false,
            window_s: 5.0,
            selection: None,
            hover_s: None,
            view: Stats::of(&[]),
        };
        app.poll_status();
        app
    }

    fn poll_status(&mut self) {
        if !self.session.is_live() || self.last_status.elapsed() < STATUS_EVERY {
            return;
        }
        self.last_status = Instant::now();
        match self.session.status() {
            Ok(s) => {
                self.status = format!("{} hw {}", s.port, s.hw);
                self.daemon_warning = client::stale_daemon(&s);
                self.mode = s.mode;
                self.voltage_mv = s.millivolts;
                self.power = s.power;
                self.measuring = s.measuring;
                self.dropped = s.dropped;
                if s.fold != self.fold {
                    self.fold = s.fold;
                    self.seconds_per_record = s.fold as f64 / SAMPLES_PER_SECOND as f64;
                }
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.session.events.try_recv() {
            match ev {
                Event::Records(r) => self.trace.extend(r),
                Event::Cleared => self.trace = Pyramid::default(),
                Event::Status(s) => self.status = s,
                Event::Closed => self.status = "daemon went away".into(),
            }
        }
    }

    fn send(&mut self, req: Request) {
        match self.session.request(&req) {
            Ok(ppk2_tool::Reply::Error { message }) => self.status = message,
            Ok(_) => {}
            Err(e) => self.status = e.to_string(),
        }
        self.last_status = Instant::now() - STATUS_EVERY;
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        self.readout_big(ui);
        ui.separator();
        if self.session.is_live() {
            self.action_buttons(ui);
            ui.separator();
            ui.heading("device");
            self.device_controls(ui);
            ui.separator();
            ui.heading("buffer");
            self.buffer_controls(ui);
            ui.separator();
        }
        ui.heading("view");
        self.view_controls(ui);
        ui.separator();
        self.readouts(ui);
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.label(egui::RichText::new(&self.status).small());
            if self.dropped > 0 {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("{} frames dropped", self.dropped),
                );
            }
            if let Some(w) = &self.daemon_warning {
                ui.colored_label(egui::Color32::RED, w);
            }
            let end_s = self.trace.len() as f64 * self.seconds_per_record;
            ui.label(
                egui::RichText::new(format!(
                    "{} records × {} samples, {}",
                    self.trace.len(),
                    self.fold,
                    fmt_duration(end_s)
                ))
                .small(),
            );
        });
    }

    fn readout_big(&self, ui: &mut egui::Ui) {
        let s = &self.view;
        ui.label(egui::RichText::new(fmt_ua(s.last)).size(30.0).monospace());
        ui.label(egui::RichText::new("live").small());
        egui::Grid::new("view_stats").num_columns(2).show(ui, |ui| {
            ui.label("mean");
            ui.monospace(fmt_ua(s.mean));
            ui.end_row();
            ui.label("min");
            ui.monospace(fmt_ua(s.min));
            ui.end_row();
            ui.label("max");
            ui.monospace(fmt_ua(s.max));
            ui.end_row();
        });
        ui.label(egui::RichText::new("of the visible window").small());
    }

    fn device_controls(&mut self, ui: &mut egui::Ui) {
        let before = self.mode;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.measuring, |ui| {
                ui.selectable_value(&mut self.mode, PowerMode::Source, "source");
                ui.selectable_value(&mut self.mode, PowerMode::Ampere, "ampere");
            });
        });
        if self.mode != before {
            self.send(Request::Set {
                mode: Some(self.mode),
                millivolts: None,
                fold: None,
            });
        }
        let slider = egui::Slider::new(&mut self.voltage_mv, 800..=5000).suffix(" mV");
        if ui
            .add_enabled(self.mode == PowerMode::Source, slider)
            .drag_stopped()
        {
            self.send(Request::Set {
                mode: None,
                millivolts: Some(self.voltage_mv),
                fold: None,
            });
        }
        let fold = egui::Slider::new(&mut self.fold, 1..=10_000)
            .logarithmic(true)
            .prefix("fold ");
        if ui.add_enabled(!self.measuring, fold).drag_stopped() {
            self.send(Request::Set {
                mode: None,
                millivolts: None,
                fold: Some(self.fold),
            });
        }
    }

    fn action_buttons(&mut self, ui: &mut egui::Ui) {
        let size = egui::vec2(ui.available_width(), 28.0);
        let power_label = if self.power { "power on" } else { "power off" };
        let power = egui::Button::selectable(self.power, power_label);
        if ui
            .add_enabled_ui(self.mode == PowerMode::Source, |ui| {
                ui.add_sized(size, power)
            })
            .inner
            .clicked()
        {
            self.power = !self.power;
            self.send(Request::Power { on: self.power });
        }
        let measure_label = if self.measuring {
            "stop measuring"
        } else {
            "measure"
        };
        if ui
            .add_sized(
                size,
                egui::Button::selectable(self.measuring, measure_label),
            )
            .clicked()
        {
            self.measuring = !self.measuring;
            self.send(if self.measuring {
                Request::Start
            } else {
                Request::Stop
            });
        }
    }

    fn buffer_controls(&mut self, ui: &mut egui::Ui) {
        if ui.button("clear").clicked() {
            self.send(Request::Clear);
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.save_path)
                .hint_text("file.ppk2")
                .desired_width(f32::INFINITY),
        );
        if ui.button("save").clicked() && !self.save_path.is_empty() {
            self.send(Request::Save {
                path: PathBuf::from(&self.save_path),
            });
        }
        if self.session.spawned_daemon() {
            ui.checkbox(&mut self.keep_daemon, "keep daemon on exit");
        }
    }

    fn view_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.follow, "follow");
            ui.toggle_value(&mut self.auto_y, "auto y");
            if ui.button("fit all").clicked() {
                self.fit_all = true;
                self.follow = false;
                self.auto_y = true;
            }
        });
        ui.add(
            egui::Slider::new(&mut self.window_s, 0.01..=86_400.0)
                .logarithmic(true)
                .suffix(" s")
                .text("window"),
        );
        ui.label(egui::RichText::new("drag: pan, scroll: zoom, shift+drag: select").small());
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
            .legend(egui_plot::Legend::default())
            .allow_scroll(false)
            .allow_drag(!ui.input(|i| i.modifiers.shift));
        let response = plot.show(ui, |p| {
            let visible_s = if self.fit_all {
                0.0..=end_s.max(self.seconds_per_record)
            } else if self.follow {
                (end_s - self.window_s).max(0.0)..=end_s.max(self.window_s)
            } else {
                let b = p.plot_bounds();
                let shift = (-b.min()[0]).max(0.0);
                (b.min()[0] + shift)..=(b.max()[0] + shift)
            };
            if self.fit_all || self.follow || *visible_s.start() == 0.0 {
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
            self.hover_s = p.pointer_coordinate().map(|c| c.x);
            self.select_with_shift_drag(p);
            if let Some(sel) = &self.selection {
                p.add(
                    Span::new("selection", sel.clone())
                        .axis(Axis::X)
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25)),
                );
            }
            stats
        });
        let zoomed = response.response.hovered() && ui.input(|i| i.zoom_delta() != 1.0);
        if response.response.dragged() || zoomed {
            self.follow = false;
            self.auto_y = false;
        }
        self.view = response.inner;
    }

    fn select_with_shift_drag(&mut self, p: &egui_plot::PlotUi) {
        let shift = p.ctx().input(|i| i.modifiers.shift);
        let r = p.response();
        let Some(x) = self.hover_s else { return };
        if shift && r.drag_started() {
            self.selection = Some(x..=x);
        } else if shift && r.dragged() {
            if let Some(sel) = &self.selection {
                let a = *sel.start();
                self.selection = Some(a.min(x)..=a.max(x));
            }
        } else if r.clicked() && !shift {
            self.selection = None;
        }
    }

    fn readouts(&self, ui: &mut egui::Ui) {
        if let Some(x) = self.hover_s {
            let i = self.to_records(x).min(self.trace.len().saturating_sub(1));
            if let Some(b) = self.trace.query(i..i + 1, 1).first() {
                ui.label(egui::RichText::new(format!("cursor @ {x:.4} s")).small());
                ui.monospace(format!(
                    "{}  [{} … {}]",
                    fmt_ua(b.mean()),
                    fmt_ua(b.min),
                    fmt_ua(b.max)
                ));
            }
        }
        if let Some(sel) = &self.selection {
            let range = self.to_records(*sel.start())..self.to_records(*sel.end());
            let s = Stats::of(&self.trace.query(range, 1));
            let dt = sel.end() - sel.start();
            ui.label(egui::RichText::new(format!("selection {}", fmt_duration(dt))).small());
            egui::Grid::new("sel_stats").num_columns(2).show(ui, |ui| {
                ui.label("mean");
                ui.monospace(fmt_ua(s.mean));
                ui.end_row();
                ui.label("min");
                ui.monospace(fmt_ua(s.min));
                ui.end_row();
                ui.label("max");
                ui.monospace(fmt_ua(s.max));
                ui.end_row();
                ui.label("charge");
                ui.monospace(fmt_charge(s.mean as f64 * dt));
                ui.end_row();
            });
        }
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
            FilledArea::new("min…max per pixel", &xs, &mins, &maxs)
                .fill_color(egui::Color32::from_rgba_unmultiplied(80, 160, 255, 60)),
        );
        p.line(
            Line::new("mean", PlotPoints::new(means))
                .color(egui::Color32::from_rgb(255, 200, 80))
                .width(1.5),
        );
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
        self.poll_status();
        egui::Panel::left("side")
            .default_size(280.0)
            .show(ui, |ui| self.side_panel(ui));
        egui::Panel::bottom("logic").show(ui, |ui| self.logic_plot(ui));
        egui::CentralPanel::default().show(ui, |ui| self.plot(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.session.shutdown(!self.keep_daemon);
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

fn fmt_duration(seconds: f64) -> String {
    match seconds {
        s if s < 1.0 => format!("{:.1} ms", s * 1e3),
        s if s < 120.0 => format!("{s:.2} s"),
        s if s < 7200.0 => format!("{:.1} min", s / 60.0),
        s => format!("{:.2} h", s / 3600.0),
    }
}

fn fmt_charge(microamp_seconds: f64) -> String {
    let uah = microamp_seconds / 3600.0;
    match uah.abs() {
        v if v.is_nan() => "–".into(),
        v if v >= 1e3 => format!("{:.3} mAh", uah / 1e3),
        _ => format!("{uah:.3} µAh"),
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
