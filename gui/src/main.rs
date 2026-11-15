mod reader;

use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;
use egui_plot::{FilledArea, Line, Plot, PlotPoints};
use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::{Bucket, Trace, find_ports};
use reader::{Control, Event};

const SECONDS_PER_SAMPLE: f64 = 1.0 / SAMPLES_PER_SECOND as f64;

fn main() -> eframe::Result {
    let port = match std::env::args().nth(1) {
        Some(p) => p,
        None => find_ports()
            .ok()
            .and_then(|p| p.into_iter().next())
            .unwrap_or_default(),
    };
    eframe::run_native(
        "ppk2_tool",
        eframe::NativeOptions::default(),
        Box::new(move |cc| Ok(Box::new(App::new(port, cc.egui_ctx.clone())))),
    )
}

struct App {
    ctl: Sender<Control>,
    events: Receiver<Event>,
    trace: Trace,
    status: String,
    voltage_mv: u16,
    power: bool,
    measuring: bool,
    follow: bool,
    window_s: f64,
}

impl App {
    fn new(port: String, ctx: egui::Context) -> Self {
        let (ctl, events) = reader::spawn(port, ctx);
        App {
            ctl,
            events,
            trace: Trace::default(),
            status: "connecting".into(),
            voltage_mv: 3300,
            power: false,
            measuring: false,
            follow: true,
            window_s: 5.0,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Samples(s) => self.trace.extend(s),
                Event::Status(s) => self.status = s,
            }
        }
    }

    fn send(&self, c: Control) {
        if self.ctl.send(c).is_err() {
            eprintln!("reader thread is gone");
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let slider = egui::Slider::new(&mut self.voltage_mv, 800..=5000).suffix(" mV");
            if ui.add(slider).drag_stopped() {
                self.send(Control::SetVoltage(self.voltage_mv));
            }
            if ui.toggle_value(&mut self.power, "power").changed() {
                self.send(Control::Power(self.power));
            }
            if ui.toggle_value(&mut self.measuring, "measure").changed() {
                self.send(Control::Measure(self.measuring));
            }
            ui.separator();
            ui.toggle_value(&mut self.follow, "follow");
            ui.add(
                egui::Slider::new(&mut self.window_s, 0.01..=600.0)
                    .logarithmic(true)
                    .suffix(" s"),
            );
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn plot(&mut self, ui: &mut egui::Ui) {
        let total = self.trace.len();
        let end_s = total as f64 * SECONDS_PER_SAMPLE;
        let plot = Plot::new("trace")
            .x_axis_label("s")
            .y_axis_label("µA")
            .auto_bounds([false, true])
            .allow_scroll(false);
        let response = plot.show(ui, |p| {
            if self.follow {
                p.set_plot_bounds_x((end_s - self.window_s)..=end_s);
            }
            let bounds = p.plot_bounds();
            let px = p.response().rect.width().max(1.0) as usize;
            let range = to_samples(bounds.min()[0], total)..to_samples(bounds.max()[0], total);
            let buckets = self.trace.query(range, px);
            draw(p, &buckets);
            Stats::of(&buckets)
        });
        if response.response.dragged()
            || response.response.hovered() && ui.input(|i| i.zoom_delta() != 1.0)
        {
            self.follow = false;
        }
        let s = response.inner;
        ui.horizontal(|ui| {
            ui.label(format!("live {}", fmt_ua(s.last)));
            ui.separator();
            ui.label(format!("mean {}", fmt_ua(s.mean)));
            ui.label(format!("min {}", fmt_ua(s.min)));
            ui.label(format!("max {}", fmt_ua(s.max)));
            ui.separator();
            ui.label(format!("{} samples", total));
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        egui::Panel::top("controls").show(ui, |ui| self.controls(ui));
        egui::CentralPanel::default().show(ui, |ui| self.plot(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.send(Control::Shutdown);
    }
}

fn to_samples(seconds: f64, total: usize) -> usize {
    ((seconds / SECONDS_PER_SAMPLE).floor().max(0.0) as usize).min(total)
}

fn draw(p: &mut egui_plot::PlotUi, buckets: &[Bucket]) {
    let xs: Vec<f64> = buckets.iter().map(mid_s).collect();
    let mins: Vec<f64> = buckets.iter().map(|b| b.min as f64).collect();
    let maxs: Vec<f64> = buckets.iter().map(|b| b.max as f64).collect();
    let means: Vec<[f64; 2]> = buckets
        .iter()
        .map(|b| [mid_s(b), b.mean() as f64])
        .collect();
    p.add(
        FilledArea::new("range", &xs, &mins, &maxs)
            .fill_color(egui::Color32::from_rgba_unmultiplied(80, 160, 255, 60)),
    );
    p.line(Line::new("mean", PlotPoints::new(means)).width(1.5));
}

fn mid_s(b: &Bucket) -> f64 {
    (b.start as f64 + b.len as f64 / 2.0) * SECONDS_PER_SAMPLE
}

struct Stats {
    last: f32,
    mean: f32,
    min: f32,
    max: f32,
}

impl Stats {
    fn of(buckets: &[Bucket]) -> Self {
        let (mut sum, mut n, mut min, mut max) = (0.0f64, 0usize, f32::INFINITY, f32::NEG_INFINITY);
        for b in buckets {
            sum += b.sum;
            n += b.len;
            min = min.min(b.min);
            max = max.max(b.max);
        }
        Stats {
            last: buckets.last().map(Bucket::mean).unwrap_or(f32::NAN),
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
