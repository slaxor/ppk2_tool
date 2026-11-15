//! Serial transport to a PPK2.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serialport::SerialPort;

use crate::Error;
use crate::cmd::{Command, PowerMode};
use crate::measure::{Converter, FrameSplitter, RawSample};
use crate::metadata::{END_MARKER, Metadata};

const BY_ID_DIR: &str = "/dev/serial/by-id";
const PPK2_PREFIX: &str = "usb-Nordic_Semiconductor_PPK2_";
const MEASUREMENT_INTERFACE: &str = "-if01";
const METADATA_TIMEOUT: Duration = Duration::from_secs(2);

/// An open PPK2 with its calibration loaded.
pub struct Ppk2 {
    port: Box<dyn SerialPort>,
    pub metadata: Metadata,
    splitter: FrameSplitter,
    converter: Converter,
}

/// Measurement ports of all attached PPK2s, via the udev by-id symlinks.
pub fn find_ports() -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(BY_ID_DIR) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut ports = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with(PPK2_PREFIX) && name.ends_with(MEASUREMENT_INTERFACE) {
            ports.push(path.to_string_lossy().into_owned());
        }
    }
    ports.sort();
    Ok(ports)
}

impl Ppk2 {
    /// Open `path`, fetch metadata, select `mode`.
    pub fn open(path: &str, mode: PowerMode) -> Result<Self, Error> {
        let mut port = serialport::new(path, 9600)
            .timeout(Duration::from_millis(50))
            .open()?;
        port.write_data_terminal_ready(true)?;
        let metadata = read_metadata(&mut *port)?;
        let converter = Converter::new(metadata.cal.clone(), metadata.vdd_mv);
        let mut dev = Ppk2 {
            port,
            metadata,
            splitter: FrameSplitter::default(),
            converter,
        };
        dev.send(Command::SetPowerMode(mode))?;
        Ok(dev)
    }

    pub fn send(&mut self, cmd: Command) -> Result<(), Error> {
        self.port.write_all(&cmd.to_bytes())?;
        Ok(())
    }

    /// Source-mode supply voltage, 800..=5000 mV.
    pub fn set_source_voltage(&mut self, millivolts: u16) -> Result<(), Error> {
        if !(800..=5000).contains(&millivolts) {
            return Err(Error::VoltageOutOfRange(millivolts));
        }
        self.send(Command::RegulatorSet { millivolts })?;
        self.converter.set_source_millivolts(millivolts);
        Ok(())
    }

    pub fn power(&mut self, on: bool) -> Result<(), Error> {
        self.send(Command::DeviceRunning(on))
    }

    pub fn start_measuring(&mut self) -> Result<(), Error> {
        self.send(Command::AverageStart)
    }

    pub fn stop_measuring(&mut self) -> Result<(), Error> {
        self.send(Command::AverageStop)
    }

    /// Read whatever is buffered and return it as calibrated microamps.
    pub fn read_microamps(&mut self, out: &mut Vec<f64>) -> Result<usize, Error> {
        let mut buf = [0u8; 4096];
        let n = match self.port.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(e.into()),
        };
        let before = out.len();
        let frames: Vec<RawSample> = self.splitter.push(&buf[..n]).collect();
        out.extend(frames.into_iter().map(|s| self.converter.microamps(s)));
        Ok(out.len() - before)
    }
}

fn read_metadata(port: &mut dyn SerialPort) -> Result<Metadata, Error> {
    port.write_all(&Command::GetMetadata.to_bytes())?;
    let mut text = String::new();
    let deadline = Instant::now() + METADATA_TIMEOUT;
    let mut buf = [0u8; 512];
    while !text.contains(END_MARKER) {
        if Instant::now() > deadline {
            return Err(Error::MetadataTimeout);
        }
        match port.read(&mut buf) {
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    text.parse()
}
