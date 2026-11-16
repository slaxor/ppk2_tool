//! Sample stream decoding and conversion to calibrated current.

use crate::metadata::{Calibration, RANGES};

/// Bytes per sample on the wire.
pub const FRAME_LEN: usize = 4;
/// Nominal sample rate of the device.
pub const SAMPLES_PER_SECOND: u32 = 100_000;

const ADC_MASK: u32 = (1 << 14) - 1;
const RANGE_SHIFT: u32 = 14;
const RANGE_MASK: u32 = 0b111;
const COUNTER_SHIFT: u32 = 18;
const COUNTER_MASK: u32 = 0b11_1111;
const LOGIC_SHIFT: u32 = 24;
const ADC_VOLTS_PER_COUNT: f64 = 1.8 / 163_840.0;
const AMPS_TO_MICROAMPS: f64 = 1e6;

/// One decoded 32-bit frame, still uncalibrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSample {
    pub adc: u16,
    pub range: u8,
    pub counter: u8,
    pub logic: u8,
}

impl RawSample {
    /// Decode a little-endian frame as sent by the firmware.
    pub fn from_frame(frame: [u8; FRAME_LEN]) -> Self {
        let word = u32::from_le_bytes(frame);
        RawSample {
            adc: (word & ADC_MASK) as u16,
            range: ((word >> RANGE_SHIFT) & RANGE_MASK) as u8,
            counter: ((word >> COUNTER_SHIFT) & COUNTER_MASK) as u8,
            logic: (word >> LOGIC_SHIFT) as u8,
        }
    }
}

/// Splits an arbitrary byte stream into whole frames, carrying leftovers,
/// and counts frames the 6-bit rolling counter says went missing.
#[derive(Debug, Default)]
pub struct FrameSplitter {
    pending: Vec<u8>,
    expected_counter: Option<u8>,
    dropped: u64,
}

impl FrameSplitter {
    /// Feed bytes; yields every complete frame in order.
    pub fn push<'a>(&'a mut self, bytes: &[u8]) -> impl Iterator<Item = RawSample> + 'a {
        self.pending.extend_from_slice(bytes);
        let whole = self.pending.len() / FRAME_LEN * FRAME_LEN;
        let frames: Vec<[u8; FRAME_LEN]> = self.pending[..whole].as_chunks().0.to_vec();
        self.pending.drain(..whole);
        frames
            .into_iter()
            .map(RawSample::from_frame)
            .inspect(|s| self.account(s.counter))
    }

    /// Frames lost between the ones delivered so far, as implied by counter gaps.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn account(&mut self, counter: u8) {
        if let Some(expected) = self.expected_counter {
            self.dropped += u64::from(counter.wrapping_sub(expected) & COUNTER_MASK as u8);
        }
        self.expected_counter = Some((counter + 1) & COUNTER_MASK as u8);
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

    fn raw(adc: u16, range: u8) -> RawSample {
        RawSample {
            adc,
            range,
            counter: 0,
            logic: 0,
        }
    }

    fn frame(adc: u32, range: u32, counter: u32, logic: u32) -> [u8; FRAME_LEN] {
        (adc | (range << RANGE_SHIFT) | (counter << COUNTER_SHIFT) | (logic << LOGIC_SHIFT))
            .to_le_bytes()
    }

    #[test]
    fn decodes_bitfields() {
        let s = RawSample::from_frame(frame(0x1abc, 3, 42, 0xa5));
        assert_eq!(
            s,
            RawSample {
                adc: 0x1abc,
                range: 3,
                counter: 42,
                logic: 0xa5
            }
        );
    }

    #[test]
    fn splitter_counts_gaps_in_the_rolling_counter() {
        let mut sp = FrameSplitter::default();
        let mut bytes = Vec::new();
        for c in [61u32, 62, 63, 0, 1, 5, 6] {
            bytes.extend_from_slice(&frame(0, 0, c, 0));
        }
        let n = sp.push(&bytes).count();
        assert_eq!(n, 7);
        assert_eq!(sp.dropped(), 3);
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
            let ua = conv.microamps(raw(adc, range as u8));
            let full_scale_ua = 1.8 / conv.cal.r[range] * AMPS_TO_MICROAMPS;
            assert!(ua.abs() < full_scale_ua * 0.02, "range {range}: {ua} uA");
        }
    }

    #[test]
    fn full_scale_of_range0_is_720_microamps_over_the_1k_shunt() {
        let mut conv = converter();
        let ua = conv.microamps(raw(16_383, 0));
        assert!((ua - 720.0).abs() < 10.0, "{ua} uA");
    }

    #[test]
    fn ranges_nest_in_decades() {
        let full: Vec<f64> = (0..RANGES)
            .map(|r| converter().microamps(raw(16_383, r as u8)))
            .collect();
        for w in full.windows(2) {
            let ratio = w[1] / w[0];
            assert!((3.0..30.0).contains(&ratio), "{full:?}");
        }
    }

    #[test]
    fn current_grows_with_adc_within_a_range() {
        let mut conv = converter();
        let lo = conv.microamps(raw(2000, 2));
        let hi = conv.microamps(raw(8000, 2));
        assert!(hi > lo, "{lo} !< {hi}");
    }

    #[test]
    fn range_switch_is_smoothed_for_three_samples() {
        let mut conv = converter();
        let steady = conv.microamps(raw(4000, 1));
        let jump = raw(12000, 2);
        let raw_jump = Converter::new(conv.cal.clone(), 3300).uncalibrated_microamps(12000, 2);
        let first = conv.microamps(jump);
        assert!((first - steady).abs() < (raw_jump - steady).abs());
        conv.microamps(jump);
        conv.microamps(jump);
        let settled = conv.microamps(jump);
        assert_eq!(settled, raw_jump);
    }
}
