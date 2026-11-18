//! The audio Plugin: the outputs and inputs, the applications playing and recording, their volume and
//! mute, the defaults, as questions, commands, streams and occurrences.
//!
//! - **`audio.status` is a question and a stream of the same record**, with a sequence: the default
//!   output and input, each with its volume and mute. A client that subscribes and then asks holds the
//!   present, and drops what the stream brings at or below the sequence it was answered with. A change is
//!   counted once, whether or not the stream flows.
//! - **`audio.devices`** is every output and input, and **`audio.streams`** every application playing or
//!   recording with the output or input it is on, each whole at every change.
//! - **The volume is the one a person reads on a slider**, as PipeWire's own tools show it, in percent: 100
//!   is a device's full scale, and up to 150 may be set.
//! - **A command is acknowledged when PipeWire has taken it**; what it changed arrives on the streams.
//! - **`audio.device.added` and `audio.device.removed`** say when an output or an input comes or goes — a
//!   headset connected, a USB card unplugged. Every occurrence carries its facts as JSON in its detail.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use yoke_sdk::plugin::{Capability, Command, Declaration, Event, Object, Outcome, Question, Severity, Stream, Unit};

use crate::backend::{AudioBackend, AudioError};
use crate::graph::{Graph, Kind, Node};

pub const PLUGIN: &str = "com.yoke.audio";
/// The format every answer, sample and detail states as `schema`.
pub const SCHEMA: u32 = 1;

pub const STATUS: &str = "audio.status";
pub const DEVICES: &str = "audio.devices";
pub const STREAMS_OF: &str = "audio.streams";
pub const QUERIES: [&str; 3] = [STATUS, DEVICES, STREAMS_OF];

pub const VOLUME_SET: &str = "audio.volume.set";
pub const MUTE_SET: &str = "audio.mute.set";
pub const DEFAULT_SET: &str = "audio.default.set";
pub const STREAM_MOVE: &str = "audio.stream.move";
pub const COMMANDS: [&str; 4] = [VOLUME_SET, MUTE_SET, DEFAULT_SET, STREAM_MOVE];

pub const STREAMS: [&str; 3] = [STATUS, DEVICES, STREAMS_OF];

pub const DEVICE_ADDED: &str = "audio.device.added";
pub const DEVICE_REMOVED: &str = "audio.device.removed";
pub const OCCURRENCES: [&str; 2] = [DEVICE_ADDED, DEVICE_REMOVED];

/// The loudest a volume may be set, as a person reads it.
pub const MAX_VOLUME_PCT: u32 = 150;
/// The longest a command waits for PipeWire's tools.
pub const COMMAND_WAIT: Duration = Duration::from_secs(10);

pub fn declaration() -> Declaration {
    let answer = |q: &&str| Capability { name: format!("query.{q}.answer"), governs: Object::Query(q.to_string()) };
    let accept = |c: &&str| Capability { name: format!("command.{c}.accept"), governs: Object::Command(c.to_string()) };
    let publish = |s: &&str| Capability { name: format!("stream.{s}.publish"), governs: Object::Stream(s.to_string()) };
    let report = |o: &&str| Capability { name: format!("event.{o}.report"), governs: Object::Occurrence(o.to_string()) };
    Declaration {
        id: PLUGIN.into(),
        needs: vec!["audio".into()],
        queries: QUERIES.iter().map(|q| q.to_string()).collect(),
        commands: COMMANDS.iter().map(|c| c.to_string()).collect(),
        streams: vec![
            // A change of the default or its volume is not one a client may miss.
            Stream { id: STATUS.into(), tolerates_loss: false, tolerates_reorder: false, max_frame: 0 },
            // The whole picture again each time, so a lost one is made good by the next.
            Stream { id: DEVICES.into(), tolerates_loss: true, tolerates_reorder: false, max_frame: 0 },
            Stream { id: STREAMS_OF.into(), tolerates_loss: true, tolerates_reorder: false, max_frame: 0 },
        ],
        occurrences: OCCURRENCES.iter().map(|o| o.to_string()).collect(),
        capabilities: QUERIES
            .iter()
            .map(answer)
            .chain(COMMANDS.iter().map(accept))
            .chain(STREAMS.iter().map(publish))
            .chain(OCCURRENCES.iter().map(report))
            .collect(),
        ..Default::default()
    }
}

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Audio(AudioError),
}

impl From<AudioError> for Error {
    fn from(e: AudioError) -> Self {
        Error::Audio(e)
    }
}

pub fn code(e: &Error) -> &'static str {
    match e {
        Error::Invalid(_) => "invalid",
        Error::Audio(AudioError::Unavailable(_)) => "unavailable",
        Error::Audio(AudioError::NotFound(_)) => "not_found",
        Error::Audio(AudioError::Refused(_)) => "backend",
    }
}

pub fn message(e: &Error) -> String {
    match e {
        Error::Invalid(m) => m.clone(),
        Error::Audio(e) => e.to_string(),
    }
}

fn input<T: DeserializeOwned>(what: &str, payload: &[u8]) -> Result<T, Error> {
    let payload = if payload.iter().all(u8::is_ascii_whitespace) { b"{}".as_slice() } else { payload };
    serde_json::from_slice(payload).map_err(|e| {
        let kind = match e.classify() {
            serde_json::error::Category::Data => "a field is missing, unknown or of the wrong type",
            _ => "it is not JSON",
        };
        Error::Invalid(format!("the payload is not what {what} takes: {kind}, at line {} column {}", e.line(), e.column()))
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoInput {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeInput {
    pub id: u32,
    pub volume_pct: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MuteInput {
    pub id: u32,
    pub mute: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdInput {
    pub id: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveInput {
    pub id: u32,
    /// The output or input to move it to, by id.
    pub to: u32,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn device_json(n: &Node, default: bool) -> Value {
    json!({
        "id": n.id, "name": n.name, "description": n.description, "bus": n.bus,
        "volume_pct": n.volume_pct, "mute": n.mute, "default": default,
    })
}

fn stream_json(g: &Graph, n: &Node) -> Value {
    let on = if n.kind == Kind::Playing { "output" } else { "input" };
    let mut v = json!({
        "id": n.id, "application": n.description, "binary": n.binary, "media": n.media,
        "volume_pct": n.volume_pct, "mute": n.mute,
    });
    v[on] = json!(g.target(n.id).map(|t| t.id));
    v
}

/// The status record's body: the defaults, with their volume and mute.
fn body(g: &Graph) -> Value {
    let brief = |n: Option<&Node>| {
        n.map(|n| json!({ "id": n.id, "name": n.name, "description": n.description, "volume_pct": n.volume_pct, "mute": n.mute }))
    };
    json!({ "output": brief(g.default_output()), "input": brief(g.default_input()) })
}

fn devices_body(g: &Graph) -> Value {
    let out = g.default_output().map(|n| n.id);
    let inp = g.default_input().map(|n| n.id);
    json!({
        "outputs": g.kind(Kind::Output).map(|n| device_json(n, Some(n.id) == out)).collect::<Vec<_>>(),
        "inputs": g.kind(Kind::Input).map(|n| device_json(n, Some(n.id) == inp)).collect::<Vec<_>>(),
    })
}

fn streams_body(g: &Graph) -> Value {
    json!({
        "playing": g.kind(Kind::Playing).map(|n| stream_json(g, n)).collect::<Vec<_>>(),
        "recording": g.kind(Kind::Recording).map(|n| stream_json(g, n)).collect::<Vec<_>>(),
    })
}

/// What the Plugin does on the Session, behind a trait so that a test can stand in for the Core.
pub trait Acts {
    fn answer(&self, q: &Question, payload: Vec<u8>) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn fail(&self, q: &Question, code: &str, message: &str) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn ack(&self, c: &Command, outcome: Outcome, line: &str) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    /// An occurrence, its facts as JSON in its detail.
    fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn emit(&self, stream: &str, payload: Vec<u8>) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
}

impl Acts for Unit {
    async fn answer(&self, q: &Question, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        Unit::answer(self, q, payload).await
    }
    async fn fail(&self, q: &Question, code: &str, message: &str) -> Result<(), yoke_sdk::base::Error> {
        Unit::fail(self, &q.id, code, message).await
    }
    async fn ack(&self, c: &Command, outcome: Outcome, line: &str) -> Result<(), yoke_sdk::base::Error> {
        Unit::ack(self, c, outcome, line).await
    }
    async fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> Result<(), yoke_sdk::base::Error> {
        Unit::report(self, occurrence, Some(Severity::of(severity)), line, detail.to_string().into_bytes()).await
    }
    async fn emit(&self, stream: &str, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        Unit::emit(self, stream, payload).await
    }
}

/// The status as last said, and what counts it.
struct Status {
    sequence: u64,
    body: Value,
    at: u64,
}

impl Status {
    fn record(&self) -> Value {
        let mut v = json!({ "schema": SCHEMA, "sequence": self.sequence, "at": self.at });
        if let (Some(v), Some(body)) = (v.as_object_mut(), self.body.as_object()) {
            v.extend(body.clone());
        }
        v
    }
}

pub type Backend = Result<Arc<dyn AudioBackend>, AudioError>;

pub struct Audio {
    pub backend: Backend,
    status: Mutex<Status>,
    /// What each whole-picture stream last emitted, so that an unchanged picture is not emitted again.
    devices: Mutex<String>,
    streams: Mutex<String>,
    /// The outputs and inputs as last read, by id, so that one coming or going is reported; `None` before
    /// the first reading, which reports nothing.
    known: Mutex<Option<BTreeMap<u32, Node>>>,
    status_flows: AtomicBool,
    devices_flow: AtomicBool,
    streams_flow: AtomicBool,
}

impl Audio {
    pub fn new(backend: Backend) -> Arc<Audio> {
        Arc::new(Audio {
            backend,
            status: Mutex::new(Status { sequence: 0, body: Value::Null, at: now_ms() }),
            devices: Mutex::new(String::new()),
            streams: Mutex::new(String::new()),
            known: Mutex::new(None),
            status_flows: AtomicBool::new(false),
            devices_flow: AtomicBool::new(false),
            streams_flow: AtomicBool::new(false),
        })
    }

    fn pipewire(&self) -> Result<&dyn AudioBackend, Error> {
        match &self.backend {
            Ok(b) => Ok(b.as_ref()),
            Err(e) => Err(Error::Audio(AudioError::Unavailable(e.to_string()))),
        }
    }

    fn flag(&self, stream: &str) -> Option<&AtomicBool> {
        match stream {
            STATUS => Some(&self.status_flows),
            DEVICES => Some(&self.devices_flow),
            STREAMS_OF => Some(&self.streams_flow),
            _ => None,
        }
    }

    /// Reads the graph and says what changed: the status counted and emitted where it flows, the devices
    /// and the streams emitted where they flow and changed, an output or an input that came or went
    /// reported. The first reading counts as sequence 1 and reports nothing.
    pub async fn refresh(&self, u: &impl Acts) -> Result<(), yoke_sdk::base::Error> {
        let Ok(b) = &self.backend else { return Ok(()) };
        let g = b.graph();
        let mut first = Ok(());
        let next = body(&g);
        let record = {
            let mut status = self.status.lock().unwrap();
            if status.body == next {
                None
            } else {
                status.body = next;
                status.sequence += 1;
                status.at = now_ms();
                Some(status.record())
            }
        };
        if let Some(record) = record {
            if self.status_flows.load(Ordering::Relaxed) {
                first = u.emit(STATUS, record.to_string().into_bytes()).await;
            }
        }
        if self.devices_flow.load(Ordering::Relaxed) {
            first = first.and(self.emit_whole(u, DEVICES, devices_body(&g), false).await);
        }
        if self.streams_flow.load(Ordering::Relaxed) {
            first = first.and(self.emit_whole(u, STREAMS_OF, streams_body(&g), false).await);
        }
        first.and(self.occurrences(u, &g).await)
    }

    async fn occurrences(&self, u: &impl Acts, g: &Graph) -> Result<(), yoke_sdk::base::Error> {
        let now: BTreeMap<u32, Node> =
            g.nodes.values().filter(|n| matches!(n.kind, Kind::Output | Kind::Input)).map(|n| (n.id, n.clone())).collect();
        let before = self.known.lock().unwrap().replace(now.clone());
        let Some(before) = before else { return Ok(()) };
        let mut first = Ok(());
        for n in now.values().filter(|n| !before.contains_key(&n.id)) {
            let line = format!("{} {} came", n.kind.as_str(), n.description);
            let detail = json!({ "schema": SCHEMA, "line": line, "id": n.id, "name": n.name, "description": n.description, "kind": n.kind.as_str(), "bus": n.bus });
            first = first.and(u.report(DEVICE_ADDED, 10, &line, detail).await);
        }
        for n in before.values().filter(|n| !now.contains_key(&n.id)) {
            let line = format!("{} {} went", n.kind.as_str(), n.description);
            let detail = json!({ "schema": SCHEMA, "line": line, "id": n.id, "name": n.name, "description": n.description, "kind": n.kind.as_str(), "bus": n.bus });
            first = first.and(u.report(DEVICE_REMOVED, 10, &line, detail).await);
        }
        first
    }

    async fn emit_whole(&self, u: &impl Acts, stream: &str, body: Value, always: bool) -> Result<(), yoke_sdk::base::Error> {
        let picture = body.to_string();
        {
            let mut last = if stream == DEVICES { self.devices.lock().unwrap() } else { self.streams.lock().unwrap() };
            if !always && *last == picture {
                return Ok(());
            }
            last.clone_from(&picture);
        }
        let mut v = json!({ "schema": SCHEMA, "at": now_ms() });
        if let (Some(v), Some(body)) = (v.as_object_mut(), body.as_object()) {
            v.extend(body.clone());
        }
        u.emit(stream, v.to_string().into_bytes()).await
    }

    /// A stream was activated: it is given the present picture at once.
    pub async fn activated(&self, u: &impl Acts, stream: &str) -> Result<(), yoke_sdk::base::Error> {
        let Some(flag) = self.flag(stream) else { return Ok(()) };
        flag.store(true, Ordering::Relaxed);
        let Ok(b) = &self.backend else { return Ok(()) };
        match stream {
            STATUS => {
                let record = self.status.lock().unwrap().record();
                u.emit(STATUS, record.to_string().into_bytes()).await
            }
            DEVICES => self.emit_whole(u, DEVICES, devices_body(&b.graph()), true).await,
            _ => self.emit_whole(u, STREAMS_OF, streams_body(&b.graph()), true).await,
        }
    }

    pub fn stopped(&self, stream: &str) {
        if let Some(flag) = self.flag(stream) {
            flag.store(false, Ordering::Relaxed);
        }
    }

    pub async fn answer(&self, query: &str, payload: &[u8]) -> Result<Vec<u8>, Error> {
        let _: NoInput = input(query, payload)?;
        let g = self.pipewire()?.graph();
        let v = match query {
            STATUS => self.status.lock().unwrap().record(),
            DEVICES | STREAMS_OF => {
                let body = if query == DEVICES { devices_body(&g) } else { streams_body(&g) };
                let mut v = json!({ "schema": SCHEMA });
                if let (Some(v), Some(body)) = (v.as_object_mut(), body.as_object()) {
                    v.extend(body.clone());
                }
                v
            }
            other => return Err(Error::Invalid(format!("{other} is not a question of {PLUGIN}"))),
        };
        Ok(v.to_string().into_bytes())
    }

    /// Carries a command out, and returns the line it is acknowledged with.
    pub async fn act(&self, command: &str, payload: &[u8]) -> Result<String, Error> {
        let b = self.pipewire()?;
        let g = b.graph();
        let node = |id: u32, kinds: &[Kind]| {
            g.nodes
                .get(&id)
                .filter(|n| kinds.contains(&n.kind))
                .cloned()
                .ok_or_else(|| Error::Audio(AudioError::NotFound(format!("there is no {} of id {id}", kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(" or ")))))
        };
        let any = [Kind::Output, Kind::Input, Kind::Playing, Kind::Recording];
        match command {
            VOLUME_SET => {
                let i: VolumeInput = input(command, payload)?;
                if i.volume_pct > MAX_VOLUME_PCT {
                    return Err(Error::Invalid(format!("a volume is at most {MAX_VOLUME_PCT}%, not {}%", i.volume_pct)));
                }
                let n = node(i.id, &any)?;
                b.set_volume(n.id, i.volume_pct).await?;
                Ok(format!("{} is at {}%", n.description, i.volume_pct))
            }
            MUTE_SET => {
                let i: MuteInput = input(command, payload)?;
                let n = node(i.id, &any)?;
                b.set_mute(n.id, i.mute).await?;
                Ok(format!("{} is {}", n.description, if i.mute { "muted" } else { "unmuted" }))
            }
            DEFAULT_SET => {
                let i: IdInput = input(command, payload)?;
                let n = node(i.id, &[Kind::Output, Kind::Input])?;
                b.set_default(n.id).await?;
                Ok(format!("{} is the default {}", n.description, n.kind.as_str()))
            }
            STREAM_MOVE => {
                let i: MoveInput = input(command, payload)?;
                let s = node(i.id, &[Kind::Playing, Kind::Recording])?;
                let fits = if s.kind == Kind::Playing { Kind::Output } else { Kind::Input };
                let to = node(i.to, &[fits])?;
                b.move_stream(s.id, &to.name).await?;
                Ok(format!("{} is on {}", s.description, to.description))
            }
            other => Err(Error::Invalid(format!("{other} is not a command of {PLUGIN}"))),
        }
    }
}

pub async fn handle(u: &impl Acts, event: &Event, a: &Audio) -> Result<(), yoke_sdk::base::Error> {
    match event {
        Event::Question(q) if QUERIES.contains(&q.r#type.as_str()) => match a.answer(&q.r#type, &q.payload).await {
            Ok(answer) => u.answer(q, answer).await,
            Err(e) => u.fail(q, code(&e), &message(&e)).await,
        },
        Event::Command(c) if COMMANDS.contains(&c.r#type.as_str()) => {
            match tokio::time::timeout(COMMAND_WAIT, a.act(&c.r#type, &c.payload)).await {
                Ok(Ok(line)) => u.ack(c, Outcome::Done, &line).await,
                Ok(Err(e)) => u.ack(c, Outcome::Failed, &format!("{}: {}", code(&e), message(&e))).await,
                Err(_) => u.ack(c, Outcome::Failed, "backend: PipeWire did not answer").await,
            }
        }
        _ => Ok(()),
    }
}
