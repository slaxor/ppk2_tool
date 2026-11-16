//! Folding raw samples into fixed-size records, and records into pyramid buckets.

use crate::device::Sample;
use crate::trace::Aggregate;

/// Size of a record on disk and in memory.
pub const RECORD_LEN: usize = 16;

/// `fold` consecutive samples summarised: what gets stored and plotted.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Record {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub logic_any: u8,
    pub logic_all: u8,
    pub dropped: u16,
}

impl Record {
    pub fn to_bytes(self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[0..4].copy_from_slice(&self.min.to_le_bytes());
        out[4..8].copy_from_slice(&self.max.to_le_bytes());
        out[8..12].copy_from_slice(&self.mean.to_le_bytes());
        out[12] = self.logic_any;
        out[13] = self.logic_all;
        out[14..16].copy_from_slice(&self.dropped.to_le_bytes());
        out
    }

    pub fn from_bytes(b: [u8; RECORD_LEN]) -> Self {
        let f32_at = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Record {
            min: f32_at(0),
            max: f32_at(4),
            mean: f32_at(8),
            logic_any: b[12],
            logic_all: b[13],
            dropped: u16::from_le_bytes([b[14], b[15]]),
        }
    }
}

/// Accumulates samples and emits a `Record` every `fold` of them.
#[derive(Debug)]
pub struct Folder {
    fold: u32,
    count: u32,
    min: f32,
    max: f32,
    sum: f64,
    logic_any: u8,
    logic_all: u8,
    dropped: u64,
}

impl Folder {
    pub fn new(fold: u32) -> Self {
        Folder {
            fold: fold.max(1),
            count: 0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum: 0.0,
            logic_any: 0,
            logic_all: 0xff,
            dropped: 0,
        }
    }

    pub fn fold(&self) -> u32 {
        self.fold
    }

    /// Frames the splitter reported missing since the last record; charged to the next one.
    pub fn note_dropped(&mut self, frames: u64) {
        self.dropped += frames;
    }

    pub fn push(&mut self, s: Sample) -> Option<Record> {
        self.count += 1;
        self.min = self.min.min(s.microamps);
        self.max = self.max.max(s.microamps);
        self.sum += s.microamps as f64;
        self.logic_any |= s.logic;
        self.logic_all &= s.logic;
        (self.count == self.fold).then(|| self.emit())
    }

    fn emit(&mut self) -> Record {
        let r = Record {
            min: self.min,
            max: self.max,
            mean: (self.sum / self.count as f64) as f32,
            logic_any: self.logic_any,
            logic_all: self.logic_all,
            dropped: self.dropped.min(u16::MAX as u64) as u16,
        };
        *self = Folder::new(self.fold);
        r
    }
}

/// Pyramid bucket over records; `start`/`len` count records, not samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordBucket {
    pub start: usize,
    pub len: usize,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
    pub logic_any: u8,
    pub logic_all: u8,
    pub dropped: u64,
}

impl RecordBucket {
    pub fn mean(&self) -> f32 {
        (self.sum / self.len as f64) as f32
    }

    pub fn toggled(&self) -> u8 {
        self.logic_any & !self.logic_all
    }
}

impl Aggregate for RecordBucket {
    type Sample = Record;

    fn empty(start: usize) -> Self {
        RecordBucket {
            start,
            len: 0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum: 0.0,
            logic_any: 0,
            logic_all: 0xff,
            dropped: 0,
        }
    }

    fn start(&self) -> usize {
        self.start
    }

    fn count(&self) -> usize {
        self.len
    }

    fn add_sample(&mut self, r: Record) {
        self.len += 1;
        self.min = self.min.min(r.min);
        self.max = self.max.max(r.max);
        self.sum += r.mean as f64;
        self.logic_any |= r.logic_any;
        self.logic_all &= r.logic_all;
        self.dropped += u64::from(r.dropped);
    }

    fn add_bucket(&mut self, b: &Self) {
        self.len += b.len;
        self.min = self.min.min(b.min);
        self.max = self.max.max(b.max);
        self.sum += b.sum;
        self.logic_any |= b.logic_any;
        self.logic_all &= b.logic_all;
        self.dropped += b.dropped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ua: f32, logic: u8) -> Sample {
        Sample {
            microamps: ua,
            logic,
            range: 0,
        }
    }

    #[test]
    fn record_round_trips_through_bytes() {
        let r = Record {
            min: -1.5,
            max: 2.25,
            mean: 0.5,
            logic_any: 0b1010,
            logic_all: 0b0010,
            dropped: 7,
        };
        assert_eq!(Record::from_bytes(r.to_bytes()), r);
        assert_eq!(std::mem::size_of::<Record>(), RECORD_LEN);
    }

    #[test]
    fn folder_emits_every_fold_samples() {
        let mut f = Folder::new(3);
        assert_eq!(f.push(sample(1.0, 0b01)), None);
        assert_eq!(f.push(sample(3.0, 0b11)), None);
        f.note_dropped(2);
        let r = f
            .push(sample(2.0, 0b01))
            .expect("third sample closes the record");
        assert_eq!(r.min, 1.0);
        assert_eq!(r.max, 3.0);
        assert_eq!(r.mean, 2.0);
        assert_eq!(r.logic_any, 0b11);
        assert_eq!(r.logic_all, 0b01);
        assert_eq!(r.dropped, 2);
        assert_eq!(f.push(sample(5.0, 0)), None);
    }

    #[test]
    fn fold_one_is_the_sample_itself() {
        let mut f = Folder::new(1);
        let r = f.push(sample(4.0, 0xff)).expect("fold 1 emits immediately");
        assert_eq!((r.min, r.max, r.mean), (4.0, 4.0, 4.0));
        assert_eq!((r.logic_any, r.logic_all), (0xff, 0xff));
    }

    #[test]
    fn bucket_mean_is_mean_of_record_means() {
        let mut b = RecordBucket::empty(0);
        for mean in [1.0, 2.0, 6.0] {
            b.add_sample(Record {
                min: mean,
                max: mean,
                mean,
                logic_any: 0,
                logic_all: 0,
                dropped: 0,
            });
        }
        assert_eq!(b.mean(), 3.0);
        assert_eq!((b.min, b.max), (1.0, 6.0));
    }
}
