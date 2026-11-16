//! Client side of the daemon protocol, plus tailing a recording file directly.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ppk2_tool::proto::{self, Frame, Reply, Request, Status};
use ppk2_tool::{Error, Reader, Record};

pub enum Event {
    Records(Vec<Record>),
    Cleared,
    Status(String),
    Closed,
}

/// Where records come from and, for a daemon, where commands go.
pub struct Session {
    control: Option<Control>,
    pub events: Receiver<Event>,
    pub fold: u32,
    spawned: Option<Child>,
}

struct Control {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

const SPAWN_WAIT: Duration = Duration::from_secs(5);
const TAIL_POLL: Duration = Duration::from_millis(200);

impl Session {
    /// Connect to a running daemon, or start one and connect to it.
    pub fn daemon(socket: &Path, ctx: egui::Context) -> Result<Self, Error> {
        let (spawned, stream) = match UnixStream::connect(socket) {
            Ok(s) => (None, s),
            Err(_) => {
                let child = spawn_daemon()?;
                (Some(child), wait_for(socket)?)
            }
        };
        let mut control = Control {
            reader: BufReader::new(stream.try_clone()?),
            writer: BufWriter::new(stream),
        };
        let status = match control.request(&Request::Status)? {
            Reply::Status(s) => s,
            other => return Err(Error::Protocol(format!("expected status, got {other:?}"))),
        };
        let events = subscribe(socket, ctx)?;
        Ok(Session {
            control: Some(control),
            events,
            fold: status.fold,
            spawned,
        })
    }

    /// Tail a recording file; no device, no controls.
    pub fn file(path: PathBuf, ctx: egui::Context) -> Result<Self, Error> {
        let mut reader = Reader::open(&path)?;
        let fold = reader.header.fold;
        let (tx, rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("ppk2-file".into())
            .spawn(move || {
                let _ = tx.send(Event::Status(format!("{}: fold {fold}", path.display())));
                loop {
                    match reader.next_batch() {
                        Ok(batch) if batch.is_empty() => thread::sleep(TAIL_POLL),
                        Ok(batch) => {
                            if tx.send(Event::Records(batch)).is_err() {
                                break;
                            }
                            ctx.request_repaint();
                        }
                        Err(e) => {
                            let _ = tx.send(Event::Status(e.to_string()));
                            break;
                        }
                    }
                }
            })?;
        Ok(Session {
            control: None,
            events: rx,
            fold,
            spawned: None,
        })
    }

    pub fn is_live(&self) -> bool {
        self.control.is_some()
    }

    pub fn spawned_daemon(&self) -> bool {
        self.spawned.is_some()
    }

    pub fn request(&mut self, req: &Request) -> Result<Reply, Error> {
        match self.control.as_mut() {
            Some(c) => c.request(req),
            None => Err(Error::Protocol("viewing a file, no daemon".into())),
        }
    }

    pub fn status(&mut self) -> Result<Status, Error> {
        match self.request(&Request::Status)? {
            Reply::Status(s) => Ok(s),
            other => Err(Error::Protocol(format!("expected status, got {other:?}"))),
        }
    }

    /// Ask the daemon to quit if we started it; a daemon we merely joined stays.
    pub fn shutdown(&mut self, stop_daemon: bool) {
        if !stop_daemon {
            return;
        }
        if let Some(child) = self.spawned.as_mut() {
            let _ = self.control.as_mut().map(|c| c.request(&Request::Quit));
            let _ = child.wait();
        }
    }
}

impl Control {
    fn request(&mut self, req: &Request) -> Result<Reply, Error> {
        proto::write_json(&mut self.writer, req)?;
        match proto::read_frame(&mut self.reader)? {
            Some(Frame::Json(json)) => proto::parse(&json),
            Some(Frame::Records(_)) => {
                Err(Error::Protocol("records on the control connection".into()))
            }
            None => Err(Error::Protocol("daemon closed the connection".into())),
        }
    }
}

fn subscribe(socket: &Path, ctx: egui::Context) -> Result<Receiver<Event>, Error> {
    let stream = UnixStream::connect(socket)?;
    let mut writer = BufWriter::new(stream.try_clone()?);
    proto::write_json(&mut writer, &Request::Subscribe { from: 0 })?;
    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ppk2-feed".into())
        .spawn(move || feed_loop(BufReader::new(stream), tx, ctx))?;
    Ok(rx)
}

fn feed_loop(mut reader: BufReader<UnixStream>, tx: Sender<Event>, ctx: egui::Context) {
    loop {
        let event = match proto::read_frame(&mut reader) {
            Ok(Some(Frame::Records(r))) => Event::Records(r),
            Ok(Some(Frame::Json(json))) => match proto::parse::<Reply>(&json) {
                Ok(Reply::Cleared) => Event::Cleared,
                Ok(other) => Event::Status(format!("{other:?}")),
                Err(e) => Event::Status(e.to_string()),
            },
            Ok(None) => Event::Closed,
            Err(e) => Event::Status(e.to_string()),
        };
        let closed = matches!(event, Event::Closed);
        if tx.send(event).is_err() || closed {
            break;
        }
        ctx.request_repaint();
    }
}

/// The `ppk2_tool` installed next to this GUI, if there is one.
pub fn sibling_daemon_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ppk2_tool")))
        .filter(|p| p.is_file())
}

/// Why the running daemon is not the binary this GUI would start, if it isn't.
pub fn stale_daemon(status: &Status) -> Option<String> {
    let ours = sibling_daemon_exe()?;
    let same_file = ours.canonicalize().ok() == Path::new(&status.exe).canonicalize().ok();
    if !same_file {
        return Some(format!(
            "daemon runs {}, not {}",
            status.exe,
            ours.display()
        ));
    }
    (proto::mtime_ms(&ours) != status.exe_mtime_ms)
        .then(|| "daemon is older than its binary on disk: restart it".to_owned())
}

fn spawn_daemon() -> Result<Child, Error> {
    let exe = sibling_daemon_exe().unwrap_or_else(|| PathBuf::from("ppk2_tool"));
    Command::new(&exe)
        .arg("serve")
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| Error::Protocol(format!("starting {}: {e}", exe.display())))
}

fn wait_for(socket: &Path) -> Result<UnixStream, Error> {
    let deadline = Instant::now() + SPAWN_WAIT;
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() > deadline => return Err(e.into()),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}
