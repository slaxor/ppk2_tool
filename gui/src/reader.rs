//! Background sources of records: a live PPK2, or a recording file being tailed.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use ppk2_tool::{Command, Folder, PowerMode, Ppk2, Reader, Record};

pub enum Control {
    SetMode(PowerMode),
    SetVoltage(u16),
    Power(bool),
    Measure(bool),
    Shutdown,
}

pub enum Event {
    Records(Vec<Record>),
    Dropped(u64),
    Status(String),
}

pub struct Source {
    pub control: Sender<Control>,
    pub events: Receiver<Event>,
    pub fold: u32,
    pub live: bool,
}

const RETRY_OPEN: Duration = Duration::from_secs(1);
const TAIL_POLL: Duration = Duration::from_millis(200);

pub fn spawn_device(port: String, fold: u32, ctx: egui::Context) -> Source {
    let (ctl_tx, ctl_rx) = std::sync::mpsc::channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ppk2-device".into())
        .spawn(move || run_device(port, fold, ctl_rx, ev_tx, ctx))
        .expect("spawning a thread only fails when the OS is out of resources");
    Source {
        control: ctl_tx,
        events: ev_rx,
        fold,
        live: true,
    }
}

pub fn spawn_file(path: PathBuf, ctx: egui::Context) -> Result<Source, ppk2_tool::Error> {
    let mut reader = Reader::open(&path)?;
    let fold = reader.header.fold;
    let (ctl_tx, ctl_rx) = std::sync::mpsc::channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ppk2-file".into())
        .spawn(move || {
            let _ = ev_tx.send(Event::Status(format!("{}: fold {fold}", path.display())));
            loop {
                if matches!(
                    ctl_rx.try_recv(),
                    Ok(Control::Shutdown) | Err(TryRecvError::Disconnected)
                ) {
                    break;
                }
                match reader.next_batch() {
                    Ok(batch) if batch.is_empty() => thread::sleep(TAIL_POLL),
                    Ok(batch) => {
                        if ev_tx.send(Event::Records(batch)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        let _ = ev_tx.send(Event::Status(e.to_string()));
                        break;
                    }
                }
            }
        })
        .expect("spawning a thread only fails when the OS is out of resources");
    Ok(Source {
        control: ctl_tx,
        events: ev_rx,
        fold,
        live: false,
    })
}

fn run_device(
    port: String,
    fold: u32,
    ctl: Receiver<Control>,
    ev: Sender<Event>,
    ctx: egui::Context,
) {
    let mut dev = loop {
        match Ppk2::open(&port, PowerMode::Source) {
            Ok(d) => break d,
            Err(e) => {
                let msg = format!("{port:?}: {e}; retrying");
                eprintln!("{msg}");
                if ev.send(Event::Status(msg)).is_err() {
                    return;
                }
                thread::sleep(RETRY_OPEN);
            }
        }
    };
    let _ = ev.send(Event::Status(format!("{port}: hw {}", dev.metadata.hw)));
    let mut folder = Folder::new(fold);
    let mut samples = Vec::new();
    let mut dropped_seen = 0;
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
        samples.clear();
        match dev.read_samples(&mut samples) {
            Ok(0) => {}
            Ok(_) => {
                let dropped = dev.dropped_frames();
                folder.note_dropped(dropped - dropped_seen);
                dropped_seen = dropped;
                let records: Vec<Record> = samples.iter().filter_map(|&s| folder.push(s)).collect();
                if !records.is_empty() && ev.send(Event::Records(records)).is_err() {
                    break;
                }
                let _ = ev.send(Event::Dropped(dropped));
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
        Control::SetMode(mode) => dev.set_power_mode(mode),
        Control::SetVoltage(mv) => dev.set_source_voltage(mv),
        Control::Power(on) => dev.power(on),
        Control::Measure(true) => dev.start_measuring(),
        Control::Measure(false) => dev.stop_measuring(),
        Control::Shutdown => Ok(()),
    }
}
