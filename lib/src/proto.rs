//! Wire protocol between the daemon (`ppk2_tool serve`) and its clients.
//!
//! Every frame is `kind: u8, len: u32 LE, payload`. Kind 0 carries one JSON
//! message, kind 1 carries `len / 16` packed records (see `record.rs`).

use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::cmd::PowerMode;
use crate::record::{RECORD_LEN, Record};

pub const KIND_JSON: u8 = 0;
pub const KIND_RECORDS: u8 = 1;
const MAX_FRAME: u32 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Set {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<PowerMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        millivolts: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fold: Option<u32>,
    },
    Power {
        on: bool,
    },
    Start,
    Stop,
    /// Drop the buffered records.
    Clear,
    Save {
        path: PathBuf,
    },
    /// Stream records from index `from`, then everything new, until the connection closes.
    Subscribe {
        from: usize,
    },
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub port: String,
    pub hw: u32,
    pub mode: PowerMode,
    pub millivolts: u16,
    pub fold: u32,
    pub power: bool,
    pub measuring: bool,
    pub records: usize,
    pub dropped: u64,
    pub started_unix_ms: Option<u64>,
    /// The daemon's own executable and its modification time when it started,
    /// so a client can tell it is talking to an outdated binary.
    pub exe: String,
    pub exe_mtime_ms: u64,
}

/// Modification time of a file in Unix milliseconds, 0 if unknown.
pub fn mtime_ms(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Error {
        message: String,
    },
    Status(Status),
    /// Sent on a subscription when the buffer was cleared; what follows starts at record 0.
    Cleared,
}

/// What arrived in one frame.
#[derive(Debug, PartialEq)]
pub enum Frame {
    Json(String),
    Records(Vec<Record>),
}

pub fn write_json(w: &mut impl Write, msg: &impl Serialize) -> Result<(), Error> {
    let body = serde_json::to_vec(msg).map_err(|e| Error::Protocol(e.to_string()))?;
    write_frame(w, KIND_JSON, &body)
}

pub fn write_records(w: &mut impl Write, records: &[Record]) -> Result<(), Error> {
    let mut body = Vec::with_capacity(records.len() * RECORD_LEN);
    for r in records {
        body.extend_from_slice(&r.to_bytes());
    }
    write_frame(w, KIND_RECORDS, &body)
}

fn write_frame(w: &mut impl Write, kind: u8, body: &[u8]) -> Result<(), Error> {
    let len = u32::try_from(body.len()).map_err(|_| Error::Protocol("frame too large".into()))?;
    w.write_all(&[kind])?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(body)?;
    w.flush()?;
    Ok(())
}

/// Blocks until one whole frame is in; `Ok(None)` on a clean close.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Frame>, Error> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let kind = head[0];
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    if len > MAX_FRAME {
        return Err(Error::Protocol(format!("frame of {len} bytes refused")));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    match kind {
        KIND_JSON => Ok(Some(Frame::Json(
            String::from_utf8(body).map_err(|e| Error::Protocol(e.to_string()))?,
        ))),
        KIND_RECORDS if body.len().is_multiple_of(RECORD_LEN) => Ok(Some(Frame::Records(
            body.chunks_exact(RECORD_LEN)
                .map(|c| Record::from_bytes(c.try_into().expect("chunks_exact yields RECORD_LEN")))
                .collect(),
        ))),
        KIND_RECORDS => Err(Error::Protocol("record frame not a multiple of 16".into())),
        other => Err(Error::Protocol(format!("unknown frame kind {other}"))),
    }
}

pub fn parse<T: for<'a> Deserialize<'a>>(json: &str) -> Result<T, Error> {
    serde_json::from_str(json).map_err(|e| Error::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_plain_json() {
        let json = serde_json::to_string(&Request::Set {
            mode: None,
            millivolts: Some(1800),
            fold: None,
        })
        .expect("serialises");
        assert_eq!(json, r#"{"cmd":"set","millivolts":1800}"#);
        assert_eq!(
            parse::<Request>(r#"{"cmd":"subscribe","from":42}"#).expect("parses"),
            Request::Subscribe { from: 42 }
        );
    }

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_json(&mut wire, &Request::Status).expect("write json");
        let records = vec![
            Record {
                min: 1.0,
                max: 2.0,
                mean: 1.5,
                logic_any: 1,
                logic_all: 0,
                dropped: 0,
            };
            3
        ];
        write_records(&mut wire, &records).expect("write records");
        let mut r = wire.as_slice();
        assert_eq!(
            read_frame(&mut r).expect("frame 1"),
            Some(Frame::Json(r#"{"cmd":"status"}"#.into()))
        );
        assert_eq!(
            read_frame(&mut r).expect("frame 2"),
            Some(Frame::Records(records))
        );
        assert_eq!(read_frame(&mut r).expect("eof"), None);
    }

    #[test]
    fn truncated_frames_are_errors_not_hangs() {
        let mut wire = Vec::new();
        write_json(&mut wire, &Request::Status).expect("write");
        wire.truncate(wire.len() - 2);
        let mut r = wire.as_slice();
        assert!(read_frame(&mut r).is_err());
    }
}
