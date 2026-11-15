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
const DRAIN_QUIET: Duration = Duration::from_millis(200);
const METADATA_START: &str = "Calibrated:";

/// One calibrated sample plus the state of the eight logic inputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub microamps: f32,
    pub logic: u8,
}

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
        quiesce(&mut *port)?;
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
