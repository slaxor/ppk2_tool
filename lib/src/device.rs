//! Serial transport to a PPK2.

use std::io::{Read, Write};
use std::path::Path;
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
const DRAIN_QUIET: Duration = Duration::from_millis(200);
const METADATA_START: &str = "Calibrated:";

/// One calibrated sample plus the state of the eight logic inputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub microamps: f32,
    pub logic: u8,
    pub range: u8,
}

/// An open PPK2 with its calibration loaded.
pub struct Ppk2 {
    port: Box<dyn SerialPort>,
    pub metadata: Metadata,
    splitter: FrameSplitter,
    converter: Converter,
    millivolts: u16,
}

/// Serial number of the PPK2 behind `port` (a by-id link or the tty it points to).
pub fn serial_of(port: &str) -> Option<String> {
    let from_name = |path: &Path| {
        let name = path.file_name()?.to_str()?;
        let rest = name.strip_prefix(PPK2_PREFIX)?;
        Some(rest.split('-').next()?.to_owned())
    };
    if let Some(serial) = from_name(Path::new(port)) {
        return Some(serial);
    }
    let target = std::fs::canonicalize(port).ok()?;
    std::fs::read_dir(BY_ID_DIR)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| std::fs::canonicalize(p).ok().as_ref() == Some(&target))
        .and_then(|p| from_name(&p))
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
    /// Open `path` and bring it up the way Nordic's app does: metadata, regulator,
    /// then the measurement mode (see `enter_mode`).
    pub fn open(path: &str, mode: PowerMode, millivolts: u16) -> Result<Self, Error> {
        let mut port = serialport::new(path, 9600)
            .timeout(Duration::from_millis(50))
            .open()?;
        port.write_data_terminal_ready(true)?;
        quiesce(&mut *port)?;
        let metadata = read_metadata(&mut *port)?;
        let converter = Converter::new(metadata.cal.clone(), metadata.vdd_mv);
        let mut dev = Ppk2 {
            port,
            metadata,
            splitter: FrameSplitter::default(),
            converter,
            millivolts,
        };
        dev.set_source_voltage(millivolts)?;
        let already_there = dev.metadata.mode == u32::from(mode.wire_code());
        match (already_there, mode) {
            (true, PowerMode::Source) => {}
            (true, PowerMode::Ampere) => dev.power(true)?,
            (false, _) => dev.enter_mode(mode)?,
        }
        Ok(dev)
    }

    /// Switch modes in the order Nordic's app uses: the source meter is entered with the
    /// output off, the ampere meter with the DUT path closed, since it only sits in series.
    pub fn enter_mode(&mut self, mode: PowerMode) -> Result<(), Error> {
        match mode {
            PowerMode::Source => {
                self.power(false)?;
                self.set_power_mode(mode)?;
                self.set_source_voltage(self.millivolts)
            }
            PowerMode::Ampere => {
                self.set_power_mode(mode)?;
                self.power(true)
            }
        }
    }

    pub fn send(&mut self, cmd: Command) -> Result<(), Error> {
        self.port.write_all(&cmd.to_bytes())?;
        Ok(())
    }

    /// Switch between source meter and ampere meter; do this while not measuring.
    pub fn set_power_mode(&mut self, mode: PowerMode) -> Result<(), Error> {
        self.send(Command::SetPowerMode(mode))
    }

    /// Source-mode supply voltage, 800..=5000 mV.
    pub fn set_source_voltage(&mut self, millivolts: u16) -> Result<(), Error> {
        if !(800..=5000).contains(&millivolts) {
            return Err(Error::VoltageOutOfRange(millivolts));
        }
        self.send(Command::RegulatorSet { millivolts })?;
        self.converter.set_source_millivolts(millivolts);
        self.millivolts = millivolts;
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

    /// Frames lost so far, from gaps in the stream's rolling counter.
    pub fn dropped_frames(&self) -> u64 {
        self.splitter.dropped()
    }

    /// Read whatever is buffered and append it as calibrated samples.
    pub fn read_samples(&mut self, out: &mut Vec<Sample>) -> Result<usize, Error> {
        let mut buf = [0u8; 4096];
        let n = match self.port.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(e) => return Err(e.into()),
        };
        let before = out.len();
        let frames: Vec<RawSample> = self.splitter.push(&buf[..n]).collect();
        out.extend(frames.into_iter().map(|s| Sample {
            microamps: self.converter.microamps(s) as f32,
            logic: s.logic,
            range: s.range,
        }));
        Ok(out.len() - before)
    }
}

/// Stop a stream a previous client may have left running and discard what it buffered.
fn quiesce(port: &mut dyn SerialPort) -> Result<(), Error> {
    port.write_all(&Command::AverageStop.to_bytes())?;
    let mut buf = [0u8; 4096];
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < DRAIN_QUIET {
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(_) => quiet_since = Instant::now(),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn read_metadata(port: &mut dyn SerialPort) -> Result<Metadata, Error> {
    port.write_all(&Command::GetMetadata.to_bytes())?;
    let mut text = String::new();
    let deadline = Instant::now() + METADATA_TIMEOUT;
    let mut buf = [0u8; 512];
    loop {
        if let Some(blob) = complete_blob(&text) {
            return blob.parse();
        }
        if Instant::now() > deadline {
            return Err(Error::MetadataTimeout);
        }
        match port.read(&mut buf) {
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// The last `Calibrated: … END` span in `text`, if one has fully arrived.
fn complete_blob(text: &str) -> Option<&str> {
    let start = text.rfind(METADATA_START)?;
    let end = text[start..].find(END_MARKER)?;
    Some(&text[start..start + end + END_MARKER.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_is_taken_from_the_last_start_marker() {
        let text = "IA: 56\nEND\n\x00\x7fCalibrated: 0\nR0: 1.0\nEND";
        assert_eq!(complete_blob(text), Some("Calibrated: 0\nR0: 1.0\nEND"));
    }

    #[test]
    fn blob_waits_for_its_own_end() {
        assert_eq!(complete_blob("END\nCalibrated: 0\nR0: 1.0\n"), None);
        assert_eq!(complete_blob(""), None);
    }
}

#[cfg(test)]
mod serial_tests {
    use super::*;

    #[test]
    fn serial_from_by_id_name() {
        assert_eq!(
            serial_of("/dev/serial/by-id/usb-Nordic_Semiconductor_PPK2_DA588140714D-if01"),
            Some("DA588140714D".to_owned())
        );
    }
}
