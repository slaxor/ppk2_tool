use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::{Folder, Header, PowerMode, Ppk2, Writer, find_ports};

#[derive(Parser)]
#[command(version, about = "Nordic PPK2 from the command line")]
struct Cli {
    /// Serial port; autodetected when omitted
    #[arg(long, global = true)]
    port: Option<String>,
    /// source: PPK2 powers the DUT; ampere: PPK2 sits in series with an external supply
    #[arg(long, global = true, value_enum, default_value_t = Mode::Source)]
    mode: Mode,
    /// Source voltage in mV
    #[arg(long, global = true, default_value_t = 3300)]
    mv: u16,
    /// Stop after this many seconds; run until Ctrl-C when omitted
    #[arg(long, global = true)]
    seconds: Option<u64>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Source,
    Ampere,
}

impl From<Mode> for PowerMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Source => PowerMode::Source,
            Mode::Ampere => PowerMode::Ampere,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Print every sample as `microamps logic` to stdout
    Stream,
    /// Fold samples into records and append them to a file
    Record {
        /// Samples per record (1 = 100 ksps, 100 = 1 ksps, ...)
        #[arg(long, default_value_t = 100)]
        fold: u32,
        file: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let port = match cli.port.clone() {
        Some(p) => p,
        None => find_ports()?
            .into_iter()
            .next()
            .ok_or("no PPK2 found; pass --port")?,
    };
    let mut dev = Ppk2::open(&port, cli.mode.into())?;
    eprintln!(
        "{port}: hw {}, fw calibrated: {}",
        dev.metadata.hw, dev.metadata.calibrated
    );
    if matches!(cli.mode, Mode::Source) {
        dev.set_source_voltage(cli.mv)?;
    }
    dev.power(true)?;
    dev.start_measuring()?;
    let result = match &cli.cmd {
        Cmd::Stream => stream(&mut dev, cli.seconds),
        Cmd::Record { fold, file } => record(&mut dev, cli.seconds, *fold, file),
    };
    dev.stop_measuring()?;
    dev.power(false)?;
    eprintln!("dropped frames: {}", dev.dropped_frames());
    result
}

struct Stop {
    until: Option<Instant>,
    interrupted: Arc<AtomicBool>,
}

impl Stop {
    fn after(seconds: Option<u64>) -> Result<Self, ctrlc::Error> {
        let interrupted = Arc::new(AtomicBool::new(false));
        let flag = interrupted.clone();
        ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))?;
        Ok(Stop {
            until: seconds.map(|s| Instant::now() + Duration::from_secs(s)),
            interrupted,
        })
    }

    fn keep_going(&self) -> bool {
        !self.interrupted.load(Ordering::Relaxed) && self.until.is_none_or(|t| Instant::now() < t)
    }
}

fn stream(dev: &mut Ppk2, seconds: Option<u64>) -> Result<(), Box<dyn std::error::Error>> {
    let stop = Stop::after(seconds)?;
    let mut out = std::io::stdout().lock();
    let mut samples = Vec::new();
    while stop.keep_going() {
        samples.clear();
        dev.read_samples(&mut samples)?;
        for s in &samples {
            writeln!(out, "{:.3} {:08b}", s.microamps, s.logic)?;
        }
    }
    Ok(())
}

fn record(
    dev: &mut Ppk2,
    seconds: Option<u64>,
    fold: u32,
    file: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let header = Header {
        fold,
        samples_per_second: SAMPLES_PER_SECOND,
        started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
    };
    let mut writer = Writer::create(file, &header)?;
    let mut folder = Folder::new(fold);
    let stop = Stop::after(seconds)?;
    let mut samples = Vec::new();
    let mut last_sync = Instant::now();
    let mut dropped_seen = 0;
    while stop.keep_going() {
        samples.clear();
        dev.read_samples(&mut samples)?;
        let dropped = dev.dropped_frames();
        folder.note_dropped(dropped - dropped_seen);
        dropped_seen = dropped;
        for s in &samples {
            if let Some(r) = folder.push(*s) {
                writer.append(r)?;
            }
        }
        if last_sync.elapsed() >= Duration::from_secs(1) {
            writer.sync()?;
            last_sync = Instant::now();
        }
    }
    writer.sync()?;
    Ok(())
}
