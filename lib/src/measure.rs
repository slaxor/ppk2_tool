//! Sample stream decoding and conversion to calibrated current.

use crate::metadata::{Calibration, RANGES};

/// Bytes per sample on the wire.
pub const FRAME_LEN: usize = 4;
/// Nominal sample rate of the device.
pub const SAMPLES_PER_SECOND: u32 = 100_000;

const ADC_MASK: u32 = (1 << 14) - 1;
const RANGE_SHIFT: u32 = 14;
const RANGE_MASK: u32 = 0b111;
const LOGIC_SHIFT: u32 = 24;
const ADC_VOLTS_PER_COUNT: f64 = 1.8 / 163_840.0;
const AMPS_TO_MICROAMPS: f64 = 1e6;

/// One decoded 32-bit frame, still uncalibrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSample {
    pub adc: u16,
    pub range: u8,
    pub logic: u8,
}

impl RawSample {
    /// Decode a little-endian frame as sent by the firmware.
    pub fn from_frame(frame: [u8; FRAME_LEN]) -> Self {
        let word = u32::from_le_bytes(frame);
        RawSample {
            adc: (word & ADC_MASK) as u16,
            range: ((word >> RANGE_SHIFT) & RANGE_MASK) as u8,
            logic: (word >> LOGIC_SHIFT) as u8,
        }
    }
}

/// Splits an arbitrary byte stream into whole frames, carrying leftovers.
#[derive(Debug, Default)]
pub struct FrameSplitter {
    pending: Vec<u8>,
}

impl FrameSplitter {
    /// Feed bytes; yields every complete frame in order.
    pub fn push<'a>(&'a mut self, bytes: &[u8]) -> impl Iterator<Item = RawSample> + 'a {
        self.pending.extend_from_slice(bytes);
        let whole = self.pending.len() / FRAME_LEN * FRAME_LEN;
        let frames: Vec<[u8; FRAME_LEN]> = self.pending[..whole]
            .chunks_exact(FRAME_LEN)
            .map(|c| c.try_into().expect("chunks_exact yields FRAME_LEN"))
            .collect();
        self.pending.drain(..whole);
        frames.into_iter().map(RawSample::from_frame)
    }
}

/// Turns raw samples into microamps using the device calibration.
#[derive(Debug, Clone)]
pub struct Converter {
    cal: Calibration,
    source_mv: f64,
    spike: SpikeFilter,
}

impl Converter {
    pub fn new(cal: Calibration, vdd_mv: u32) -> Self {
        Converter {
            cal,
            source_mv: vdd_mv as f64,
            spike: SpikeFilter::default(),
        }
    }

    /// Tell the converter what the regulator is set to (source mode).
    pub fn set_source_millivolts(&mut self, mv: u16) {
        self.source_mv = mv as f64;
    }

    /// Calibrated current in microamps, spike-filtered across range switches.
    pub fn microamps(&mut self, sample: RawSample) -> f64 {
        let range = (sample.range as usize).min(RANGES - 1);
        let raw = self.uncalibrated_microamps(sample.adc, range);
        self.spike.apply(range, raw)
    }

    fn uncalibrated_microamps(&self, adc: u16, range: usize) -> f64 {
        let c = &self.cal;
        let counts = adc as f64 * 4.0 - c.o[range];
        let amps_linear = counts * (ADC_VOLTS_PER_COUNT / c.r[range]);
        let gain = c.gs[range] * amps_linear + c.gi[range];
        let offset = c.s[range] * (self.source_mv / 1000.0) + c.i[range];
        c.ug[range] * (amps_linear * gain + offset) * AMPS_TO_MICROAMPS
    }
}

/// Reference-implementation smoothing for the first samples after a range switch.
#[derive(Debug, Clone, Default)]
struct SpikeFilter {
    prev_range: Option<usize>,
    after_spike: u8,
    consecutive: u32,
    rolling: f64,
    rolling_r4: f64,
}

const ALPHA: f64 = 0.18;
const ALPHA_R4: f64 = 0.06;
const SETTLE_SAMPLES: u8 = 3;

impl SpikeFilter {
    fn apply(&mut self, range: usize, value: f64) -> f64 {
        let Some(prev) = self.prev_range else {
            self.prev_range = Some(range);
            self.rolling = value;
            self.rolling_r4 = value;
            return value;
        };
        self.prev_range = Some(range);
        if prev == range && self.after_spike == 0 {
            self.rolling = value;
            self.rolling_r4 = value;
            return value;
        }
        if prev != range {
            self.consecutive = 0;
            self.after_spike = SETTLE_SAMPLES;
        } else {
            self.consecutive += 1;
        }
        self.rolling = if range == RANGES - 1 && self.consecutive < 2 {
            let out = self.rolling_r4;
            self.rolling_r4 = value;
            out
        } else {
            let alpha = if range == RANGES - 1 { ALPHA_R4 } else { ALPHA };
            let smoothed = alpha * value + (1.0 - alpha) * self.rolling;
            self.rolling_r4 = smoothed;
            smoothed
        };
        self.after_spike -= 1;
        self.rolling
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{Metadata, SAMPLE_BLOB};

    fn converter() -> Converter {
        let meta: Metadata = SAMPLE_BLOB.parse().expect("blob parses");
        Converter::new(meta.cal, meta.vdd_mv)
    }

    #[test]
    fn decodes_bitfields() {
        let adc: u32 = 0x1abc;
        let range: u32 = 3;
        let logic: u32 = 0xa5;
        let word = adc | (range << RANGE_SHIFT) | (logic << LOGIC_SHIFT);
        let s = RawSample::from_frame(word.to_le_bytes());
        assert_eq!(
            s,
            RawSample {
                adc: 0x1abc,
                range: 3,
                logic: 0xa5
            }
        );
    }

    #[test]
    fn splitter_carries_partial_frames() {
        let mut sp = FrameSplitter::default();
        let bytes: Vec<u8> = (0u8..10).collect();
        let first: Vec<_> = sp.push(&bytes[..5]).collect();
        assert_eq!(first.len(), 1);
        let second: Vec<_> = sp.push(&bytes[5..]).collect();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0], RawSample::from_frame([4, 5, 6, 7]));
    }

    #[test]
    fn adc_at_offset_reads_only_the_static_offset() {
        let mut conv = converter();
        for range in 0..RANGES {
            let adc = (conv.cal.o[range] / 4.0).round() as u16;
            let ua = conv.microamps(RawSample {
                adc,
                range: range as u8,
                logic: 0,
            });
            let full_scale_ua = 1.8 / conv.cal.r[range] * AMPS_TO_MICROAMPS;
            assert!(ua.abs() < full_scale_ua * 0.02, "range {range}: {ua} uA");
        }
    }

    #[test]
    fn full_scale_of_range0_is_720_microamps_over_the_1k_shunt() {
        let mut conv = converter();
        let ua = conv.microamps(RawSample {
            adc: 16_383,
            range: 0,
            logic: 0,
        });
        assert!((ua - 720.0).abs() < 10.0, "{ua} uA");
    }

    #[test]
    fn ranges_nest_in_decades() {
        let full: Vec<f64> = (0..RANGES)
            .map(|r| {
                converter().microamps(RawSample {
                    adc: 16_383,
                    range: r as u8,
                    logic: 0,
                })
            })
            .collect();
        for w in full.windows(2) {
            let ratio = w[1] / w[0];
            assert!((3.0..30.0).contains(&ratio), "{full:?}");
        }
    }

    #[test]
    fn current_grows_with_adc_within_a_range() {
        let mut conv = converter();
        let lo = conv.microamps(RawSample {
            adc: 2000,
            range: 2,
            logic: 0,
        });
        let hi = conv.microamps(RawSample {
            adc: 8000,
            range: 2,
            logic: 0,
        });
        assert!(hi > lo, "{lo} !< {hi}");
    }

    #[test]
    fn range_switch_is_smoothed_for_three_samples() {
        let mut conv = converter();
        let steady = conv.microamps(RawSample {
            adc: 4000,
            range: 1,
            logic: 0,
        });
        let jump = RawSample {
            adc: 12000,
            range: 2,
            logic: 0,
        };
        let raw_jump = Converter::new(conv.cal.clone(), 3300).uncalibrated_microamps(12000, 2);
        let first = conv.microamps(jump);
        assert!((first - steady).abs() < (raw_jump - steady).abs());
        conv.microamps(jump);
        conv.microamps(jump);
        let settled = conv.microamps(jump);
        assert_eq!(settled, raw_jump);
    }
}
