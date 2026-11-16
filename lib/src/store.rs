//! Recording file: a text header line, then packed records.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::Error;
use crate::record::{RECORD_LEN, Record};

const MAGIC: &str = "ppk2_tool";
const VERSION: u32 = 1;

/// What a recording needs to be interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub fold: u32,
    pub samples_per_second: u32,
    pub started_unix_ms: u64,
}

impl Header {
    fn to_line(&self) -> String {
        format!(
            "{MAGIC} v{VERSION} fold={} sps={} started_ms={}\n",
            self.fold, self.samples_per_second, self.started_unix_ms
        )
    }

    fn parse_line(line: &str) -> Result<Self, Error> {
        let bad = || Error::BadRecording(line.trim().to_owned());
        let mut words = line.split_whitespace();
        if words.next() != Some(MAGIC) || words.next() != Some(&format!("v{VERSION}")) {
            return Err(bad());
        }
        let (mut fold, mut sps, mut started) = (None, None, None);
        for w in words {
            let (k, v) = w.split_once('=').ok_or_else(bad)?;
            match k {
                "fold" => fold = Some(v.parse().map_err(|_| bad())?),
                "sps" => sps = Some(v.parse().map_err(|_| bad())?),
                "started_ms" => started = Some(v.parse().map_err(|_| bad())?),
                _ => {}
            }
        }
        Ok(Header {
            fold: fold.ok_or_else(bad)?,
            samples_per_second: sps.ok_or_else(bad)?,
            started_unix_ms: started.ok_or_else(bad)?,
        })
    }
}

pub struct Writer {
    out: BufWriter<File>,
}

impl Writer {
    pub fn create(path: &Path, header: &Header) -> Result<Self, Error> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(header.to_line().as_bytes())?;
        out.flush()?;
        Ok(Writer { out })
    }

    pub fn append(&mut self, r: Record) -> Result<(), Error> {
        self.out.write_all(&r.to_bytes())?;
        Ok(())
    }

    /// Hand buffered records to the OS so readers of the file see them now.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.out.flush()?;
        Ok(())
    }

    /// Push buffered records to the OS and the disk.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.out.flush()?;
        self.out.get_ref().sync_data()?;
        Ok(())
    }
}

/// Reads a recording, including one still being written: `next_batch` returns what's new.
pub struct Reader {
    file: BufReader<File>,
    pub header: Header,
    pending: Vec<u8>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut file = BufReader::new(File::open(path)?);
        let mut line = String::new();
        file.read_line(&mut line)?;
        let header = Header::parse_line(&line)?;
        Ok(Reader {
            file,
            header,
            pending: Vec::new(),
        })
    }

    /// Every complete record appended since the last call.
    pub fn next_batch(&mut self) -> Result<Vec<Record>, Error> {
        let mut buf = Vec::new();
        self.file.read_to_end(&mut buf)?;
        self.pending.extend_from_slice(&buf);
        let whole = self.pending.len() / RECORD_LEN * RECORD_LEN;
        let records = self.pending[..whole]
            .as_chunks()
            .0
            .iter()
            .copied()
            .map(Record::from_bytes)
            .collect();
        self.pending.drain(..whole);
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(mean: f32) -> Record {
        Record {
            min: mean - 1.0,
            max: mean + 1.0,
            mean,
            logic_any: 1,
            logic_all: 0,
            dropped: 0,
        }
    }

    #[test]
    fn header_round_trips() {
        let h = Header {
            fold: 100,
            samples_per_second: 100_000,
            started_unix_ms: 1_700_000_000_123,
        };
        assert_eq!(
            Header::parse_line(&h.to_line()).expect("own line parses"),
            h
        );
    }

    #[test]
    fn rejects_foreign_files() {
        let err = Header::parse_line("#!/bin/sh\n").expect_err("not ours");
        assert!(matches!(err, Error::BadRecording(_)));
    }

    #[test]
    fn reader_tails_a_file_being_written() {
        let dir = std::env::temp_dir().join(format!("ppk2_tool-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("rec.ppk2");
        let header = Header {
            fold: 10,
            samples_per_second: 100_000,
            started_unix_ms: 0,
        };
        let mut w = Writer::create(&path, &header).expect("create");
        w.append(record(1.0)).expect("append");
        w.sync().expect("sync");

        let mut r = Reader::open(&path).expect("open");
        assert_eq!(r.header, header);
        assert_eq!(r.next_batch().expect("batch 1"), vec![record(1.0)]);
        assert_eq!(r.next_batch().expect("empty batch"), vec![]);

        w.append(record(2.0)).expect("append");
        w.append(record(3.0)).expect("append");
        w.sync().expect("sync");
        assert_eq!(
            r.next_batch().expect("batch 2"),
            vec![record(2.0), record(3.0)]
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
