//! PipeWire, reached through its own tools: `pw-dump --monitor` prints the graph and then every change of
//! it, so the Plugin follows the graph without reading it again on a clock; `wpctl` sets a volume, a mute
//! and the default; `pw-metadata` moves a stream. Behind a trait, so that a test stands in for it.
//!
//! The tools reach the PipeWire the environment names — `PIPEWIRE_REMOTE`, which the Core sets for a unit
//! that needs `audio` — and are looked for where a distribution puts them, never on a `PATH`.

use std::io::BufReader;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::graph::{Graph, Objects};

#[derive(Debug)]
pub enum AudioError {
    /// PipeWire, or its tools, cannot be reached.
    Unavailable(String),
    /// No node of that id, or not one of the kind the command takes.
    NotFound(String),
    /// The tool said no.
    Refused(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::Unavailable(m) | AudioError::NotFound(m) | AudioError::Refused(m) => f.write_str(m),
        }
    }
}

#[async_trait]
pub trait AudioBackend: Send + Sync + 'static {
    /// The graph as it stands.
    fn graph(&self) -> Graph;
    /// Told once at every change of the graph.
    fn subscribe(&self) -> broadcast::Receiver<()>;
    /// Sets a node's volume, as a person reads it, in percent.
    async fn set_volume(&self, id: u32, volume_pct: u32) -> Result<(), AudioError>;
    async fn set_mute(&self, id: u32, mute: bool) -> Result<(), AudioError>;
    /// Makes an output or an input the default.
    async fn set_default(&self, id: u32) -> Result<(), AudioError>;
    /// Plays a stream on another output, or records it from another input, named by PipeWire's name.
    async fn move_stream(&self, stream: u32, to: &str) -> Result<(), AudioError>;
}

/// Where a tool is: the first of a distribution's places that holds it.
fn tool(name: &str) -> Result<String, AudioError> {
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
        .ok_or_else(|| AudioError::Unavailable(format!("{name} is not installed")))
}

pub struct PipeWire {
    objects: Arc<Mutex<Objects>>,
    changed: broadcast::Sender<()>,
    wpctl: String,
    metadata: String,
}

impl PipeWire {
    /// Starts following the graph. Returns once the first picture is held, or with the reason none could
    /// be; a `pw-dump` that ends later says so on `ended`.
    pub fn start(ended: tokio::sync::oneshot::Sender<String>) -> Result<Arc<PipeWire>, AudioError> {
        let dump = tool("pw-dump")?;
        let wpctl = tool("wpctl")?;
        let metadata = tool("pw-metadata")?;
        let mut child = Command::new(&dump)
            .args(["--monitor", "--no-colors"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AudioError::Unavailable(format!("pw-dump cannot be started: {e}")))?;
        let stdout = child.stdout.take().expect("pw-dump's output is piped");
        let objects = Arc::new(Mutex::new(Objects::default()));
        let (changed, _) = broadcast::channel(16);
        let (first_tx, first_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        {
            let (objects, changed) = (objects.clone(), changed.clone());
            std::thread::Builder::new()
                .name("pw-dump".into())
                .spawn(move || {
                    let mut first = Some(first_tx);
                    // pw-dump prints one JSON array after another: the whole graph, then each change.
                    for batch in serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<Value>() {
                        let Ok(batch) = batch else { break };
                        objects.lock().unwrap().apply(&batch);
                        match first.take() {
                            Some(tx) => {
                                let _ = tx.send(Ok(()));
                            }
                            None => {
                                let _ = changed.send(());
                            }
                        }
                    }
                    let status = child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                    let why = format!("pw-dump ended ({status}): PipeWire cannot be reached");
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Err(why));
                    } else {
                        let _ = ended.send(why);
                    }
                })
                .map_err(|e| AudioError::Unavailable(e.to_string()))?;
        }
        match first_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(PipeWire { objects, changed, wpctl, metadata })),
            Ok(Err(why)) => Err(AudioError::Unavailable(why)),
            Err(_) => Err(AudioError::Unavailable("pw-dump said nothing".into())),
        }
    }

    async fn run(&self, program: &str, args: &[String]) -> Result<(), AudioError> {
        let out = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| AudioError::Unavailable(format!("{program} cannot be run: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(AudioError::Refused(if said.is_empty() { out.status.to_string() } else { said }))
        }
    }
}

#[async_trait]
impl AudioBackend for PipeWire {
    fn graph(&self) -> Graph {
        self.objects.lock().unwrap().graph()
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    async fn set_volume(&self, id: u32, volume_pct: u32) -> Result<(), AudioError> {
        let level = format!("{:.2}", volume_pct as f64 / 100.0);
        self.run(&self.wpctl, &["set-volume".into(), id.to_string(), level]).await
    }

    async fn set_mute(&self, id: u32, mute: bool) -> Result<(), AudioError> {
        self.run(&self.wpctl, &["set-mute".into(), id.to_string(), if mute { "1" } else { "0" }.into()]).await
    }

    async fn set_default(&self, id: u32) -> Result<(), AudioError> {
        self.run(&self.wpctl, &["set-default".into(), id.to_string()]).await
    }

    async fn move_stream(&self, stream: u32, to: &str) -> Result<(), AudioError> {
        self.run(&self.metadata, &[stream.to_string(), "target.object".into(), to.to_string()]).await
    }
}
