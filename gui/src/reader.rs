//! Background thread that owns the device and streams samples to the UI.

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;

use ppk2_tool::{Command, PowerMode, Ppk2};

pub enum Control {
    SetVoltage(u16),
    Power(bool),
    Measure(bool),
    Shutdown,
}

pub enum Event {
    Samples(Vec<f32>),
    Status(String),
}

pub fn spawn(port: String, ctx: egui::Context) -> (Sender<Control>, Receiver<Event>) {
    let (ctl_tx, ctl_rx) = std::sync::mpsc::channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ppk2-reader".into())
        .spawn(move || run(port, ctl_rx, ev_tx, ctx))
        .expect("spawning a thread only fails when the OS is out of resources");
    (ctl_tx, ev_rx)
}

fn run(port: String, ctl: Receiver<Control>, ev: Sender<Event>, ctx: egui::Context) {
    let mut dev = match Ppk2::open(&port, PowerMode::Source) {
        Ok(d) => d,
        Err(e) => {
            let _ = ev.send(Event::Status(format!("{port}: {e}")));
            return;
        }
    };
    let _ = ev.send(Event::Status(format!("{port}: hw {}", dev.metadata.hw)));
    let mut microamps = Vec::new();
    loop {
        match ctl.try_recv() {
            Ok(Control::Shutdown) | Err(TryRecvError::Disconnected) => break,
            Ok(c) => {
                if let Err(e) = apply(&mut dev, c) {
                    let _ = ev.send(Event::Status(e.to_string()));
                }
            }
            Err(TryRecvError::Empty) => {}
        }
        microamps.clear();
        match dev.read_microamps(&mut microamps) {
            Ok(0) => {}
            Ok(_) => {
                let chunk = microamps.iter().map(|&ua| ua as f32).collect();
                if ev.send(Event::Samples(chunk)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
            Err(e) => {
                let _ = ev.send(Event::Status(e.to_string()));
                break;
            }
        }
    }
    let _ = dev.send(Command::AverageStop);
    let _ = dev.send(Command::DeviceRunning(false));
}

fn apply(dev: &mut Ppk2, c: Control) -> Result<(), ppk2_tool::Error> {
    match c {
        Control::SetVoltage(mv) => dev.set_source_voltage(mv),
        Control::Power(on) => dev.power(on),
        Control::Measure(true) => dev.start_measuring(),
        Control::Measure(false) => dev.stop_measuring(),
        Control::Shutdown => Ok(()),
    }
}
