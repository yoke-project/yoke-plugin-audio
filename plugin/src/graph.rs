//! The audio graph as PipeWire describes it, read from the objects `pw-dump` prints: the outputs and the
//! inputs, the applications playing and recording, which output each plays to, and the defaults.
//!
//! Nothing here runs anything: it folds objects into a picture, so that a test can hand it objects.

use std::collections::BTreeMap;

use serde_json::Value;

/// What a node is to a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A device sound is played on: `Audio/Sink`.
    Output,
    /// A device sound is recorded from: `Audio/Source`.
    Input,
    /// An application playing: `Stream/Output/Audio`.
    Playing,
    /// An application recording: `Stream/Input/Audio`.
    Recording,
}

impl Kind {
    fn of(media_class: &str) -> Option<Kind> {
        match media_class {
            "Audio/Sink" => Some(Kind::Output),
            "Audio/Source" => Some(Kind::Input),
            "Stream/Output/Audio" => Some(Kind::Playing),
            "Stream/Input/Audio" => Some(Kind::Recording),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Output => "output",
            Kind::Input => "input",
            Kind::Playing => "playing",
            Kind::Recording => "recording",
        }
    }
}

/// One node of the graph a person has a reason to see.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: u32,
    pub kind: Kind,
    /// PipeWire's name for it, which the defaults name: `alsa_output.pci-0000_04_00.6.analog-stereo`.
    pub name: String,
    /// What a person reads: the device's description, or the application's name.
    pub description: String,
    /// The application's program, for a stream.
    pub binary: Option<String>,
    /// What the application says it is playing or recording: `Playback`, a song's title.
    pub media: Option<String>,
    /// The bus of the device it belongs to: `pci`, `usb`, `bluetooth`.
    pub bus: Option<String>,
    /// The volume as a person reads it on a slider, as PipeWire's own tools show it: the cube root of the
    /// channels' linear volume, in percent, 100 being the device's full scale.
    pub volume_pct: u32,
    pub mute: bool,
}

/// The picture: every node, the links between them, and the defaults by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: BTreeMap<u32, Node>,
    /// A stream's id, and the ids of the nodes it is linked to.
    pub links: BTreeMap<u32, Vec<u32>>,
    pub default_output: Option<String>,
    pub default_input: Option<String>,
}

impl Graph {
    pub fn kind(&self, kind: Kind) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(move |n| n.kind == kind)
    }

    pub fn by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.values().find(|n| n.name == name)
    }

    pub fn default_output(&self) -> Option<&Node> {
        self.default_output.as_deref().and_then(|n| self.by_name(n)).filter(|n| n.kind == Kind::Output)
    }

    pub fn default_input(&self) -> Option<&Node> {
        self.default_input.as_deref().and_then(|n| self.by_name(n)).filter(|n| n.kind == Kind::Input)
    }

    /// The output or input a stream is linked to, where it is linked to one.
    pub fn target(&self, stream: u32) -> Option<&Node> {
        self.links
            .get(&stream)?
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .find(|n| matches!(n.kind, Kind::Output | Kind::Input))
    }
}

/// The volume a person reads, from PipeWire's linear volume of each channel.
pub fn volume_pct(channels: &[f64]) -> u32 {
    if channels.is_empty() {
        return 0;
    }
    let linear = channels.iter().sum::<f64>() / channels.len() as f64;
    (linear.max(0.0).cbrt() * 100.0).round() as u32
}

/// The objects `pw-dump` has printed so far, by id, folded into a picture on demand.
#[derive(Debug, Default)]
pub struct Objects {
    by_id: BTreeMap<u64, Value>,
}

impl Objects {
    /// Takes one batch `pw-dump` printed: an object with an `info` (or `props`) of null was removed, any
    /// other replaces what was held, its `info` merged so that a change of one parameter keeps the rest.
    pub fn apply(&mut self, batch: &Value) {
        for o in batch.as_array().into_iter().flatten() {
            let Some(id) = o.get("id").and_then(Value::as_u64) else { continue };
            let removed = o.get("info").is_some_and(Value::is_null) || o.get("props").is_some_and(Value::is_null);
            if removed {
                self.by_id.remove(&id);
                continue;
            }
            match self.by_id.get_mut(&id) {
                Some(held) => merge(held, o),
                None => {
                    self.by_id.insert(id, o.clone());
                }
            }
        }
    }

    pub fn graph(&self) -> Graph {
        let mut g = Graph::default();
        let devices: BTreeMap<u64, &Value> = self
            .by_id
            .iter()
            .filter(|(_, o)| kind_of(o) == "Device")
            .map(|(id, o)| (*id, o))
            .collect();
        for (id, o) in &self.by_id {
            match kind_of(o) {
                "Node" => {
                    let props = &o["info"]["props"];
                    let Some(kind) = props["media.class"].as_str().and_then(Kind::of) else { continue };
                    let text = |k: &str| props[k].as_str().map(str::to_string);
                    let name = text("node.name").unwrap_or_default();
                    let description = match kind {
                        Kind::Output | Kind::Input => text("node.description").or(text("node.nick")),
                        Kind::Playing | Kind::Recording => text("application.name").or(text("node.description")),
                    }
                    .unwrap_or_else(|| name.clone());
                    let bus = props["device.id"]
                        .as_u64()
                        .and_then(|d| devices.get(&d))
                        .and_then(|d| d["info"]["props"]["device.bus"].as_str().or(d["info"]["props"]["device.api"].as_str()))
                        .map(|b| if b == "bluez5" { "bluetooth".to_string() } else { b.to_string() });
                    let p = o["info"]["params"]["Props"].as_array().and_then(|a| a.first());
                    let channels: Vec<f64> = p
                        .and_then(|p| p["channelVolumes"].as_array())
                        .map(|a| a.iter().filter_map(Value::as_f64).collect())
                        .unwrap_or_default();
                    let mute = p.and_then(|p| p["mute"].as_bool()).unwrap_or(false);
                    g.nodes.insert(
                        *id as u32,
                        Node {
                            id: *id as u32,
                            kind,
                            name,
                            description,
                            binary: text("application.process.binary"),
                            media: text("media.name"),
                            bus,
                            volume_pct: volume_pct(&channels),
                            mute,
                        },
                    );
                }
                "Link" => {
                    let info = &o["info"];
                    if let (Some(from), Some(to)) = (info["output-node-id"].as_u64(), info["input-node-id"].as_u64()) {
                        // A stream playing is linked from itself to an output; one recording, from an
                        // input to itself. Either way the stream is the key.
                        g.links.entry(from as u32).or_default().push(to as u32);
                        g.links.entry(to as u32).or_default().push(from as u32);
                    }
                }
                "Metadata" if o["props"]["metadata.name"] == "default" => {
                    for m in o["metadata"].as_array().into_iter().flatten() {
                        let name = m["value"]["name"].as_str().map(str::to_string);
                        match m["key"].as_str() {
                            Some("default.audio.sink") => g.default_output = name,
                            Some("default.audio.source") => g.default_input = name,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        for links in g.links.values_mut() {
            links.sort_unstable();
            links.dedup();
        }
        g
    }
}

fn kind_of(o: &Value) -> &str {
    o["type"].as_str().and_then(|t| t.rsplit(':').next()).unwrap_or("")
}

/// Merges what `pw-dump` printed again of an object into what was held: an object field by field, a
/// value replaced. A metadata object is printed with the entries that changed alone, so its entries are
/// merged by subject and key, one whose value is null taking the entry away.
fn merge(held: &mut Value, new: &Value) {
    match (held, new) {
        (Value::Object(h), Value::Object(n)) => {
            for (k, v) in n {
                match h.get_mut(k) {
                    Some(slot) if k == "metadata" => merge_entries(slot, v),
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        h.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (held, new) => *held = new.clone(),
    }
}

fn merge_entries(held: &mut Value, new: &Value) {
    let Some(entries) = held.as_array_mut() else {
        *held = new.clone();
        return;
    };
    for e in new.as_array().into_iter().flatten() {
        let same = |h: &Value| h["subject"] == e["subject"] && h["key"] == e["key"];
        entries.retain(|h| !same(h));
        if !e["value"].is_null() {
            entries.push(e.clone());
        }
    }
}
