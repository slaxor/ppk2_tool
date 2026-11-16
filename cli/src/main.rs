mod serve;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::settings::socket_path;
use ppk2_tool::{
    Folder, Header, PowerMode, Ppk2, RegulatorCalibration, Settings, Writer, find_ports, serial_of,
};

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
    /// Seconds between switching the DUT on and the first sample; 0 to capture the switch-on
    #[arg(long, global = true, default_value_t = 0.5)]
    warmup: f64,
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
    /// Measure for a while (default 2 s) and print how the samples spread over the ranges
    Probe,
    /// Fold samples into records and append them to a file
    Record {
        /// Samples per record (1 = 100 ksps, 100 = 1 ksps, ...)
        #[arg(long, default_value_t = 100)]
        fold: u32,
        file: PathBuf,
    },
    /// Measure the source regulator at two voltages with a multimeter and store the correction
    CalibrateVoltage {
        /// Lower calibration point in mV
        #[arg(long, default_value_t = 1800)]
        low: u16,
        /// Upper calibration point in mV; keep it below what anything on VOUT tolerates
        #[arg(long, default_value_t = 3300)]
        high: u16,
    },
    /// Own the device and serve clients (the GUI) over a unix socket
    Serve {
        /// Socket path; default $XDG_RUNTIME_DIR/ppk2_tool.sock
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Where the current buffer lives; default $XDG_RUNTIME_DIR/ppk2_tool (tmpfs)
        #[arg(long)]
        spool: Option<PathBuf>,
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
    if let Cmd::Serve { socket, spool } = cli.cmd {
        let socket = socket.unwrap_or_else(socket_path);
        let spool = spool.unwrap_or_else(|| socket.with_extension(""));
        return serve::run(port, socket, spool);
    }
    if let Cmd::CalibrateVoltage { low, high } = cli.cmd {
        return calibrate_voltage(&port, low, high);
    }
    let regulator =
        RegulatorCalibration::load(&serial_of(&port).unwrap_or_default())?.command_for(cli.mv);
    let mut dev = Ppk2::open(&port, cli.mode.into(), regulator)?;
    eprintln!(
        "{port}: hw {}, fw calibrated: {}",
        dev.metadata.hw, dev.metadata.calibrated
    );
    dev.power(true)?;
    std::thread::sleep(Duration::from_secs_f64(cli.warmup.max(0.0)));
    dev.start_measuring()?;
    let result = match &cli.cmd {
        Cmd::Stream => stream(&mut dev, cli.seconds),
        Cmd::Probe => probe(&mut dev, cli.seconds.unwrap_or(2)),
        Cmd::Record { fold, file } => record(&mut dev, cli.seconds, *fold, file),
        Cmd::Serve { .. } | Cmd::CalibrateVoltage { .. } => {
            unreachable!("handled before the device was opened")
        }
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

fn calibrate_voltage(port: &str, low: u16, high: u16) -> Result<(), Box<dyn std::error::Error>> {
    if std::os::unix::net::UnixStream::connect(socket_path()).is_ok() {
        return Err("a ppk2_tool daemon is running; quit it (and the GUI) first".into());
    }
    let serial = serial_of(port).ok_or("cannot tell the PPK2's serial number from the port")?;
    let wanted = Settings::load()?.millivolts;
    eprintln!(
        "VOUT will go to {low} mV and then {high} mV. Leave nothing on VOUT that can't take {high} mV;
         a resistor or a multimeter alone is fine. Measure between VOUT and GND."
    );
    let mut dev = Ppk2::open(port, PowerMode::Source, low)?;
    dev.power(true)?;
    let result = (|| {
        let low_mv = read_voltage(&mut dev, low)?;
        let high_mv = read_voltage(&mut dev, high)?;
        Ok::<_, Box<dyn std::error::Error>>(RegulatorCalibration::from_points(
            (low, low_mv),
            (high, high_mv),
        )?)
    })();
    dev.power(false)?;
    let cal = result?;
    cal.save(&serial)?;
    eprintln!(
        "gain {:.5}, offset {:+.1} mV; {wanted} mV is now sent as {} mV. Saved to {}",
        cal.gain,
        cal.offset_mv,
        cal.command_for(wanted),
        RegulatorCalibration::path(&serial).display()
    );
    Ok(())
}

fn read_voltage(dev: &mut Ppk2, command_mv: u16) -> Result<f64, Box<dyn std::error::Error>> {
    dev.set_source_voltage(command_mv)?;
    eprint!("regulator at {command_mv} mV; multimeter reads (V or mV): ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    parse_reading(&line).ok_or_else(|| format!("not a voltage: {:?}", line.trim()).into())
}

/// "3.267", "3,267" and "3267" all mean 3267 mV.
fn parse_reading(text: &str) -> Option<f64> {
    let value: f64 = text.trim().replace(',', ".").parse().ok()?;
    Some(if value < 10.0 { value * 1000.0 } else { value })
}

#[derive(Default)]
struct RangeTally {
    count: u64,
    sum: f64,
    min: f32,
    max: f32,
}

impl RangeTally {
    fn add(&mut self, ua: f32) {
        if self.count == 0 {
            (self.min, self.max) = (ua, ua);
        }
        self.count += 1;
        self.sum += ua as f64;
        self.min = self.min.min(ua);
        self.max = self.max.max(ua);
    }
}

fn probe(dev: &mut Ppk2, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let stop = Stop::after(Some(seconds))?;
    let mut tallies: [RangeTally; 8] = Default::default();
    let mut total = RangeTally::default();
    let mut samples = Vec::new();
    while stop.keep_going() {
        samples.clear();
        dev.read_samples(&mut samples)?;
        for s in &samples {
            tallies[s.range as usize & 7].add(s.microamps);
            total.add(s.microamps);
        }
    }
    println!("range  samples     share  mean µA      min µA       max µA");
    for (range, t) in tallies.iter().enumerate().filter(|(_, t)| t.count > 0) {
        println!(
            "{range:>5}  {:>9}  {:>6.2}%  {:>11.3}  {:>11.3}  {:>11.3}",
            t.count,
            100.0 * t.count as f64 / total.count.max(1) as f64,
            t.sum / t.count as f64,
            t.min,
            t.max
        );
    }
    println!(
        "  all  {:>9}           {:>11.3}  {:>11.3}  {:>11.3}",
        total.count,
        total.sum / total.count.max(1) as f64,
        total.min,
        total.max
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_in_volts_or_millivolts() {
        let cases = [
            ("3.267\n", Some(3267.0)),
            ("3,267", Some(3267.0)),
            ("3267", Some(3267.0)),
            (" 1.8 ", Some(1800.0)),
            ("abc", None),
        ];
        for (text, want) in cases {
            assert_eq!(parse_reading(text).map(f64::round), want, "{text:?}");
        }
    }
}
