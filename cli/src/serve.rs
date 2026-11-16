//! The daemon: owns the device, spools records to a file, answers clients on a unix socket.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ppk2_tool::measure::SAMPLES_PER_SECOND;
use ppk2_tool::proto::{self, Frame, Reply, Request, Status};
use ppk2_tool::{Error, Folder, Header, PowerMode, Ppk2, Reader, Settings, Writer};

const SYNC_EVERY: Duration = Duration::from_secs(1);
const TAIL_POLL: Duration = Duration::from_millis(100);

struct State {
    port: String,
    hw: u32,
    settings: Settings,
    power: bool,
    measuring: bool,
    records: usize,
    dropped: u64,
    started_unix_ms: Option<u64>,
    spool_dir: PathBuf,
    generation: u64,
}

impl State {
    fn spool_file(&self) -> PathBuf {
        self.spool_dir
            .join(format!("current-{}.ppk2", self.generation))
    }

    fn status(&self) -> Status {
        Status {
            port: self.port.clone(),
            hw: self.hw,
            mode: self.settings.mode,
            millivolts: self.settings.millivolts,
            fold: self.settings.fold,
            power: self.power,
            measuring: self.measuring,
            records: self.records,
            dropped: self.dropped,
            started_unix_ms: self.started_unix_ms,
        }
    }
}

type Shared = Arc<Mutex<State>>;

enum DevCmd {
    SetMode(PowerMode),
    SetVoltage(u16),
    SetFold(u32),
    Power(bool),
    Measure(bool),
    Clear,
    Sync,
    Quit,
}

struct Job {
    cmd: DevCmd,
    done: Sender<Result<(), String>>,
}

pub fn run(
    port: String,
    socket: PathBuf,
    spool_dir: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::load()?;
    std::fs::create_dir_all(&spool_dir)?;
    let dev = Ppk2::open(&port, settings.mode)?;
    let state = Arc::new(Mutex::new(State {
        port: port.clone(),
        hw: dev.metadata.hw,
        settings,
        power: false,
        measuring: false,
        records: 0,
        dropped: 0,
        started_unix_ms: None,
        spool_dir,
        generation: 0,
    }));
    let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<Job>();
    let listener = bind(&socket)?;
    eprintln!(
        "{port}: hw {}, listening on {}",
        dev.metadata.hw,
        socket.display()
    );

    let accept_state = state.clone();
    let accept_jobs = jobs_tx.clone();
    thread::Builder::new()
        .name("ppk2-accept".into())
        .spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => spawn_client(stream, accept_state.clone(), accept_jobs.clone()),
                    Err(e) => eprintln!("accept: {e}"),
                }
            }
        })?;

    let result = device_loop(dev, state, jobs_rx);
    let _ = std::fs::remove_file(&socket);
    result.map_err(Into::into)
}

fn bind(socket: &Path) -> Result<UnixListener, Error> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(Error::Protocol(format!(
                "{} is already served",
                socket.display()
            )));
        }
        std::fs::remove_file(socket)?;
    }
    Ok(UnixListener::bind(socket)?)
}

fn device_loop(mut dev: Ppk2, state: Shared, jobs: Receiver<Job>) -> Result<(), Error> {
    let (mut writer, mut folder) = open_spool(&state)?;
    let mut samples = Vec::new();
    let mut dropped_seen = 0;
    let mut last_sync = Instant::now();
    loop {
        match jobs.try_recv() {
            Ok(job) => {
                let quit = matches!(job.cmd, DevCmd::Quit);
                let outcome = apply(&mut dev, &state, &mut writer, &mut folder, job.cmd);
                let _ = job.done.send(outcome.map_err(|e| e.to_string()));
                if quit {
                    break;
                }
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        samples.clear();
        dev.read_samples(&mut samples)?;
        let dropped = dev.dropped_frames();
        folder.note_dropped(dropped - dropped_seen);
        dropped_seen = dropped;
        let mut appended = 0;
        for s in &samples {
            if let Some(r) = folder.push(*s) {
                writer.append(r)?;
                appended += 1;
            }
        }
        if appended > 0 {
            writer.flush()?;
        }
        if appended > 0 || dropped > 0 {
            let mut st = state.lock().expect("state mutex poisoned");
            st.records += appended;
            st.dropped = dropped;
        }
        if last_sync.elapsed() >= SYNC_EVERY {
            writer.sync()?;
            last_sync = Instant::now();
        }
    }
    writer.sync()?;
    let _ = dev.stop_measuring();
    let _ = dev.power(false);
    Ok(())
}

fn open_spool(state: &Shared) -> Result<(Writer, Folder), Error> {
    let mut st = state.lock().expect("state mutex poisoned");
    let header = Header {
        fold: st.settings.fold,
        samples_per_second: SAMPLES_PER_SECOND,
        started_unix_ms: now_ms(),
    };
    st.records = 0;
    st.started_unix_ms = Some(header.started_unix_ms);
    let writer = Writer::create(&st.spool_file(), &header)?;
    Ok((writer, Folder::new(header.fold)))
}

fn apply(
    dev: &mut Ppk2,
    state: &Shared,
    writer: &mut Writer,
    folder: &mut Folder,
    cmd: DevCmd,
) -> Result<(), Error> {
    match cmd {
        DevCmd::SetMode(mode) => {
            dev.set_power_mode(mode)?;
            persist(state, |s| s.mode = mode)
        }
        DevCmd::SetVoltage(mv) => {
            dev.set_source_voltage(mv)?;
            persist(state, |s| s.millivolts = mv)
        }
        DevCmd::SetFold(fold) => {
            persist(state, |s| s.fold = fold.max(1))?;
            restart_spool(state, writer, folder)
        }
        DevCmd::Power(on) => {
            dev.power(on)?;
            with_state(state, |st| st.power = on);
            Ok(())
        }
        DevCmd::Measure(on) => {
            if on {
                dev.start_measuring()?
            } else {
                dev.stop_measuring()?
            }
            with_state(state, |st| st.measuring = on);
            Ok(())
        }
        DevCmd::Clear => restart_spool(state, writer, folder),
        DevCmd::Sync => writer.sync(),
        DevCmd::Quit => Ok(()),
    }
}

fn restart_spool(state: &Shared, writer: &mut Writer, folder: &mut Folder) -> Result<(), Error> {
    writer.sync()?;
    let old = with_state(state, |st| {
        let old = st.spool_file();
        st.generation += 1;
        old
    });
    let (w, f) = open_spool(state)?;
    *writer = w;
    *folder = f;
    let _ = std::fs::remove_file(old);
    Ok(())
}

fn with_state<R>(state: &Shared, f: impl FnOnce(&mut State) -> R) -> R {
    f(&mut state.lock().expect("state mutex poisoned"))
}

fn persist(state: &Shared, change: impl FnOnce(&mut Settings)) -> Result<(), Error> {
    with_state(state, |st| {
        change(&mut st.settings);
        st.settings.clone()
    })
    .save()
}

fn spawn_client(stream: UnixStream, state: Shared, jobs: Sender<Job>) {
    let _ = thread::Builder::new()
        .name("ppk2-client".into())
        .spawn(move || {
            if let Err(e) = serve_client(stream, state, jobs) {
                eprintln!("client: {e}");
            }
        });
}

fn serve_client(stream: UnixStream, state: Shared, jobs: Sender<Job>) -> Result<(), Error> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    while let Some(frame) = proto::read_frame(&mut reader)? {
        let Frame::Json(json) = frame else {
            proto::write_json(&mut writer, &error("clients send JSON frames only"))?;
            continue;
        };
        let request: Request = match proto::parse(&json) {
            Ok(r) => r,
            Err(e) => {
                proto::write_json(&mut writer, &error(&e.to_string()))?;
                continue;
            }
        };
        match request {
            Request::Subscribe { from } => return stream_records(&mut writer, &state, from),
            other => {
                let reply = handle(other, &state, &jobs);
                proto::write_json(&mut writer, &reply)?;
            }
        }
    }
    Ok(())
}

fn handle(request: Request, state: &Shared, jobs: &Sender<Job>) -> Reply {
    let cmd = match request {
        Request::Status => return Reply::Status(with_state(state, |st| st.status())),
        Request::Set {
            mode,
            millivolts,
            fold,
        } => {
            let steps = [
                mode.map(DevCmd::SetMode),
                millivolts.map(DevCmd::SetVoltage),
                fold.map(DevCmd::SetFold),
            ];
            for cmd in steps.into_iter().flatten() {
                if let Err(e) = submit(jobs, cmd) {
                    return error(&e);
                }
            }
            return Reply::Ok;
        }
        Request::Power { on } => DevCmd::Power(on),
        Request::Start => DevCmd::Measure(true),
        Request::Stop => DevCmd::Measure(false),
        Request::Clear => DevCmd::Clear,
        Request::Save { path } => {
            return match submit(jobs, DevCmd::Sync).and_then(|()| save_to(state, &path)) {
                Ok(()) => Reply::Ok,
                Err(e) => error(&e),
            };
        }
        Request::Quit => DevCmd::Quit,
        Request::Subscribe { .. } => return error("subscribe is handled per connection"),
    };
    match submit(jobs, cmd) {
        Ok(()) => Reply::Ok,
        Err(e) => error(&e),
    }
}

fn submit(jobs: &Sender<Job>, cmd: DevCmd) -> Result<(), String> {
    let (done, outcome) = std::sync::mpsc::channel();
    jobs.send(Job { cmd, done })
        .map_err(|_| "device thread is gone".to_string())?;
    outcome
        .recv()
        .map_err(|_| "device thread dropped the job".to_string())?
}

fn save_to(state: &Shared, path: &Path) -> Result<(), String> {
    let spool = with_state(state, |st| st.spool_file());
    std::fs::copy(&spool, path)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn stream_records(
    out: &mut BufWriter<UnixStream>,
    state: &Shared,
    from: usize,
) -> Result<(), Error> {
    let mut generation = with_state(state, |st| st.generation);
    let mut reader = Reader::open(&with_state(state, |st| st.spool_file()))?;
    let mut skip = from;
    loop {
        let current = with_state(state, |st| st.generation);
        if current != generation {
            generation = current;
            reader = Reader::open(&with_state(state, |st| st.spool_file()))?;
            skip = 0;
            proto::write_json(out, &Reply::Cleared)?;
        }
        let mut batch = reader.next_batch()?;
        if skip > 0 {
            let n = skip.min(batch.len());
            batch.drain(..n);
            skip -= n;
        }
        if batch.is_empty() {
            thread::sleep(TAIL_POLL);
            continue;
        }
        proto::write_records(out, &batch)?;
    }
}

fn error(message: &str) -> Reply {
    Reply::Error {
        message: message.to_owned(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
