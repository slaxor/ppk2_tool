//! Append-only sample store with a min/max/mean pyramid for fast zoomable views.

use std::ops::Range;

/// Samples per bucket at level 1; each further level groups `FANOUT` buckets.
pub const FANOUT: usize = 64;

/// Aggregate of a run of consecutive samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    pub start: usize,
    pub len: usize,
    pub min: f32,
    pub max: f32,
    pub sum: f64,
}

impl Bucket {
    fn empty(start: usize) -> Self {
        Bucket {
            start,
            len: 0,
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            sum: 0.0,
        }
    }

    fn add_sample(&mut self, v: f32) {
        self.len += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v as f64;
    }

    fn add_bucket(&mut self, b: &Bucket) {
        self.len += b.len;
        self.min = self.min.min(b.min);
        self.max = self.max.max(b.max);
        self.sum += b.sum;
    }

    pub fn mean(&self) -> f32 {
        (self.sum / self.len as f64) as f32
    }
}

#[derive(Debug, Default)]
pub struct Trace {
    raw: Vec<f32>,
    levels: Vec<Vec<Bucket>>,
}

impl Trace {
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn push(&mut self, v: f32) {
        self.raw.push(v);
        let idx = self.raw.len() - 1;
        if idx.is_multiple_of(FANOUT) {
            self.open_bucket(0, idx);
        }
        let b = self.last_bucket_mut(0);
        b.add_sample(v);
        if b.len == FANOUT {
            let closed = *b;
            self.promote(1, closed);
        }
    }

    pub fn extend(&mut self, vs: impl IntoIterator<Item = f32>) {
        vs.into_iter().for_each(|v| self.push(v));
    }

    fn open_bucket(&mut self, level: usize, start: usize) {
        if self.levels.len() == level {
            self.levels.push(Vec::new());
        }
        self.levels[level].push(Bucket::empty(start));
    }

    fn last_bucket_mut(&mut self, level: usize) -> &mut Bucket {
        self.levels[level]
            .last_mut()
            .expect("open_bucket ran before last_bucket_mut")
    }

    fn promote(&mut self, level: usize, closed: Bucket) {
        let size = level_size(level);
        if closed.start.is_multiple_of(size) {
            self.open_bucket(level, closed.start);
        }
        let b = self.last_bucket_mut(level);
        b.add_bucket(&closed);
        if b.len == size {
            let up = *b;
            self.promote(level + 1, up);
        }
    }

    /// About `px` buckets covering `range`, from the coarsest level that still resolves it.
    pub fn query(&self, range: Range<usize>, px: usize) -> Vec<Bucket> {
        let range = range.start.min(self.raw.len())..range.end.min(self.raw.len());
        if range.is_empty() || px == 0 {
            return Vec::new();
        }
        let per_px = range.len().div_ceil(px).max(1);
        let level = coarsest_level_within(per_px, self.levels.len());
        let mut out: Vec<Bucket> = Vec::with_capacity(px);
        let flush_into = |out: &mut Vec<Bucket>, b: &Bucket| {
            let slot = (b.start - range.start) / per_px;
            match out.last_mut() {
                Some(last) if (last.start - range.start) / per_px == slot => last.add_bucket(b),
                _ => out.push(*b),
            }
        };
        let Some(level) = level else {
            for i in range.clone() {
                let mut b = Bucket::empty(i);
                b.add_sample(self.raw[i]);
                flush_into(&mut out, &b);
            }
            return out;
        };
        let size = level_size(level);
        let first = range.start / size;
        let last = range.end.div_ceil(size);
        for b in self.levels[level].iter().take(last).skip(first) {
            if b.start + b.len <= range.start || b.start >= range.end {
                continue;
            }
            flush_into(&mut out, b);
        }
        out
    }
}

fn level_size(level: usize) -> usize {
    FANOUT.pow(level as u32 + 1)
}

fn coarsest_level_within(samples: usize, levels: usize) -> Option<usize> {
    (0..levels).rev().find(|&l| level_size(l) <= samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Trace {
        let mut t = Trace::default();
        t.extend((0..n).map(|i| i as f32));
        t
    }

    #[test]
    fn pyramid_levels_close_at_fanout_boundaries() {
        let mut t = ramp(FANOUT * FANOUT + 5);
        assert_eq!(t.levels[0].len(), FANOUT + 1);
        assert_eq!(t.levels[1].len(), 1);
        t.extend((0..FANOUT).map(|_| 0.0));
        assert_eq!(t.levels[1].len(), 2);
        assert_eq!(t.levels[1][0].len, FANOUT * FANOUT);
        assert_eq!(t.levels[1][0].min, 0.0);
        assert_eq!(t.levels[1][0].max, (FANOUT * FANOUT - 1) as f32);
    }

    #[test]
    fn raw_query_returns_every_sample() {
        let t = ramp(1000);
        let out = t.query(10..20, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out[3].min, 13.0);
        assert_eq!(out[3].len, 1);
    }

    #[test]
    fn coarse_query_covers_range_without_gaps() {
        let n = FANOUT * FANOUT * 10;
        let t = ramp(n);
        let out = t.query(0..n, 100);
        assert!(out.len() <= 100 + 1, "{}", out.len());
        let covered: usize = out.iter().map(|b| b.len).sum();
        assert_eq!(covered, n);
        assert_eq!(out[0].min, 0.0);
        assert_eq!(out.last().map(|b| b.max), Some((n - 1) as f32));
        for w in out.windows(2) {
            assert_eq!(w[0].start + w[0].len, w[1].start);
        }
    }

    #[test]
    fn mean_is_preserved_through_aggregation() {
        let n = FANOUT * 1000;
        let t = ramp(n);
        let out = t.query(0..n, 7);
        let total: f64 = out.iter().map(|b| b.sum).sum();
        assert_eq!(total, (0..n).map(|i| i as f64).sum::<f64>());
    }

    #[test]
    fn query_clamps_to_available_samples() {
        let t = ramp(10);
        assert!(t.query(20..30, 5).is_empty());
        assert_eq!(t.query(5..100, 5).len(), 5);
    }
}
