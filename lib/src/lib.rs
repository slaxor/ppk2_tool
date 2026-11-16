//! Host-side driver for Nordic's Power Profiler Kit II.

pub mod cmd;
pub mod device;
pub mod measure;
pub mod metadata;
pub mod proto;
pub mod record;
pub mod settings;
pub mod store;
pub mod trace;

pub use cmd::{Command, PowerMode};
pub use device::{Ppk2, Sample, find_ports};
pub use measure::{Converter, RawSample};
pub use metadata::Metadata;
pub use proto::{Reply, Request, Status};
pub use record::{Folder, Record, RecordBucket};
pub use settings::Settings;
pub use store::{Header, Reader, Writer};
pub use trace::{Aggregate, Bucket, Pyramid, Trace};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serial port: {0}")]
    SerialPort(#[from] serialport::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("metadata field missing: {0}")]
    MissingField(String),
    #[error("metadata line not `key: number`: {0:?}")]
    MalformedLine(String),
    #[error("no END marker within timeout")]
    MetadataTimeout,
    #[error("source voltage {0} mV outside 800..=5000")]
    VoltageOutOfRange(u16),
    #[error("not a ppk2_tool recording: {0:?}")]
    BadRecording(String),
    #[error("settings: {0}")]
    BadSettings(String),
    #[error("protocol: {0}")]
    Protocol(String),
}
