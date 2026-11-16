//! The calibration blob returned by `GetMetadata`.

use std::collections::HashMap;
use std::str::FromStr;

use crate::Error;

/// Number of measurement ranges the ADC front end switches between.
pub const RANGES: usize = 5;

/// Per-range calibration constants, indexed by range 0..=4.
#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    pub r: [f64; RANGES],
    pub gs: [f64; RANGES],
    pub gi: [f64; RANGES],
    pub o: [f64; RANGES],
    pub s: [f64; RANGES],
    pub i: [f64; RANGES],
    pub ug: [f64; RANGES],
}

/// Everything the device reports about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub calibrated: bool,
    pub vdd_mv: u32,
    pub hw: u32,
    pub mode: u32,
    pub ia: u32,
    pub cal: Calibration,
}

/// Terminator the firmware sends after the last key.
pub const END_MARKER: &str = "END";

impl FromStr for Metadata {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let fields = parse_fields(text)?;
        let scalar = |key: &str| -> Result<f64, Error> {
            fields
                .get(key)
                .copied()
                .ok_or_else(|| Error::MissingField(key.to_owned()))
        };
        let per_range = |prefix: &str| -> Result<[f64; RANGES], Error> {
            let mut out = [0.0; RANGES];
            for (idx, slot) in out.iter_mut().enumerate() {
                *slot = scalar(&format!("{prefix}{idx}"))?;
            }
            Ok(out)
        };
        Ok(Metadata {
            calibrated: scalar("Calibrated")? != 0.0,
            vdd_mv: scalar("VDD")? as u32,
            hw: scalar("HW")? as u32,
            mode: scalar("mode")? as u32,
            ia: scalar("IA")? as u32,
            cal: Calibration {
                r: per_range("R")?,
                gs: per_range("GS")?,
                gi: per_range("GI")?,
                o: per_range("O")?,
                s: per_range("S")?,
                i: per_range("I")?,
                ug: per_range("UG")?,
            },
        })
    }
}

fn parse_fields(text: &str) -> Result<HashMap<&str, f64>, Error> {
    let mut fields = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line == END_MARKER {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| Error::MalformedLine(line.to_owned()))?;
        let value: f64 = value
            .trim()
            .parse()
            .map_err(|_| Error::MalformedLine(line.to_owned()))?;
        fields.insert(key.trim(), value);
    }
    Ok(fields)
}

#[cfg(test)]
pub(crate) const SAMPLE_BLOB: &str = include_str!("../testdata/metadata_DA588140714D.txt");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_device_blob() {
        let meta: Metadata = SAMPLE_BLOB.parse().expect("real blob parses");
        assert!(!meta.calibrated);
        assert_eq!(meta.vdd_mv, 3300);
        assert_eq!(meta.hw, 24455);
        assert_eq!(meta.mode, 1, "metadata was taken in ampere-meter mode");
        assert_eq!(meta.ia, 56);
        assert_eq!(meta.cal.r[0], 997.7660522460938);
        assert_eq!(meta.cal.r[4], 0.056569237262010574);
        assert_eq!(meta.cal.o[2], 75.05016326904297);
        assert_eq!(meta.cal.ug, [1.0; RANGES]);
    }

    #[test]
    fn rejects_missing_keys() {
        let err = "R0: 1.0\nEND".parse::<Metadata>().expect_err("incomplete");
        assert!(matches!(err, Error::MissingField(_)), "{err:?}");
    }

    #[test]
    fn rejects_garbage_lines() {
        let err = "no colon here".parse::<Metadata>().expect_err("garbage");
        assert!(matches!(err, Error::MalformedLine(_)), "{err:?}");
    }
}
