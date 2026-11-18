//! The Plugin against a PipeWire that is not there: objects as `pw-dump` prints them, folded by the
//! Plugin's own graph, stand in for the daemon, and a recorder for the Core.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use audio::backend::{AudioBackend, AudioError};
use audio::graph::{volume_pct, Graph, Kind, Objects};
use audio::plugin::*;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use yoke_sdk::plugin::{Command, Event, Outcome, Question};

fn node(id: u64, class: &str, name: &str, props: Value, channels: &[f64], mute: bool) -> Value {
    let mut p = json!({ "media.class": class, "node.name": name });
    p.as_object_mut().unwrap().extend(props.as_object().unwrap().clone());
    json!({
        "id": id, "type": "PipeWire:Interface:Node",
        "info": { "props": p, "params": { "Props": [ { "channelVolumes": channels, "mute": mute } ] } },
    })
}

fn defaults(sink: &str, source: &str) -> Value {
    json!({
        "id": 40, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
        "metadata": [
            { "subject": 0, "key": "default.audio.sink", "value": { "name": sink } },
            { "subject": 0, "key": "default.audio.source", "value": { "name": source } },
        ],
    })
}

/// A laptop: speakers and a microphone on PCI, a headset on Bluetooth, and a browser playing on the headset.
fn laptop() -> Value {
    json!([
        { "id": 10, "type": "PipeWire:Interface:Device", "info": { "props": { "device.api": "alsa", "device.bus": "pci" } } },
        { "id": 11, "type": "PipeWire:Interface:Device", "info": { "props": { "device.api": "bluez5" } } },
        node(20, "Audio/Sink", "speakers", json!({ "node.description": "Speakers", "device.id": 10 }), &[0.125, 0.125], false),
        node(21, "Audio/Source", "mic", json!({ "node.description": "Microphone", "device.id": 10 }), &[1.0, 1.0], false),
        node(22, "Audio/Sink", "headset", json!({ "node.description": "Headset", "device.id": 11 }), &[0.216, 0.216], false),
        node(30, "Stream/Output/Audio", "Browser", json!({ "application.name": "Browser", "application.process.binary": "browser", "media.name": "Playback" }), &[1.0, 1.0], false),
        node(31, "Video/Source", "camera", json!({}), &[], false),
        { "id": 50, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 30, "input-node-id": 22, "state": "active" } },
        defaults("headset", "mic"),
    ])
}

/// PipeWire as the Plugin sees it: the objects it was given, and what it was asked to do.
struct Fake {
    objects: Mutex<Objects>,
    asked: Mutex<Vec<String>>,
    changed: broadcast::Sender<()>,
}

impl Fake {
    fn new() -> Arc<Fake> {
        let mut objects = Objects::default();
        objects.apply(&laptop());
        Arc::new(Fake { objects: Mutex::new(objects), asked: Mutex::new(vec![]), changed: broadcast::channel(4).0 })
    }
    fn print(&self, batch: Value) {
        self.objects.lock().unwrap().apply(&batch);
    }
    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

#[async_trait]
impl AudioBackend for Fake {
    fn graph(&self) -> Graph {
        self.objects.lock().unwrap().graph()
    }
    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }
    async fn set_volume(&self, id: u32, volume_pct: u32) -> Result<(), AudioError> {
        self.asked.lock().unwrap().push(format!("volume {id} {volume_pct}"));
        Ok(())
    }
    async fn set_mute(&self, id: u32, mute: bool) -> Result<(), AudioError> {
        self.asked.lock().unwrap().push(format!("mute {id} {mute}"));
        Ok(())
    }
    async fn set_default(&self, id: u32) -> Result<(), AudioError> {
        self.asked.lock().unwrap().push(format!("default {id}"));
        Ok(())
    }
    async fn move_stream(&self, stream: u32, to: &str) -> Result<(), AudioError> {
        self.asked.lock().unwrap().push(format!("move {stream} {to}"));
        Ok(())
    }
}

#[derive(Default)]
struct Core {
    said: Mutex<Vec<String>>,
}

impl Core {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.said.lock().unwrap())
    }
    fn push(&self, line: String) -> Result<(), yoke_sdk::base::Error> {
        self.said.lock().unwrap().push(line);
        Ok(())
    }
}

impl Acts for Core {
    async fn answer(&self, _q: &Question, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("answer {}", String::from_utf8_lossy(&payload)))
    }
    async fn fail(&self, _q: &Question, code: &str, message: &str) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("fail {code}: {message}"))
    }
    async fn ack(&self, _c: &Command, outcome: Outcome, line: &str) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("ack {outcome:?}: {line}"))
    }
    async fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> Result<(), yoke_sdk::base::Error> {
        assert_eq!(detail["line"], line, "the detail carries the line a client reads");
        self.push(format!("report {occurrence} {severity}: {line}"))
    }
    async fn emit(&self, stream: &str, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("emit {stream} {}", String::from_utf8_lossy(&payload)))
    }
}

fn question(r#type: &str) -> Event {
    Event::Question(Question { id: "q-1".into(), r#type: r#type.into(), payload: b"{}".to_vec() })
}

fn command(r#type: &str, payload: Value) -> Event {
    Event::Command(Command { id: "c-1".into(), r#type: r#type.into(), payload: payload.to_string().into_bytes() })
}

fn json_of(line: &str, skip: usize) -> Value {
    serde_json::from_str(line.splitn(skip + 1, ' ').nth(skip).unwrap()).unwrap()
}

#[test]
fn the_manifest_kept_is_the_manifest_declared() {
    assert_eq!(declaration().manifest(), include_str!("../com.yoke.audio.yaml"));
}

#[test]
fn every_object_declared_has_one_capability_and_audio_is_needed() {
    let d = declaration();
    assert_eq!(d.capabilities.len(), d.queries.len() + d.commands.len() + d.streams.len() + d.occurrences.len());
    assert_eq!(d.needs, vec!["audio".to_string()]);
}

#[test]
fn the_graph_is_what_pw_dump_printed_with_the_volume_a_person_reads() {
    let mut objects = Objects::default();
    objects.apply(&laptop());
    let g = objects.graph();
    // The camera is no node of audio's.
    assert_eq!(g.nodes.keys().copied().collect::<Vec<_>>(), vec![20, 21, 22, 30]);
    assert_eq!(volume_pct(&[0.125, 0.125]), 50, "the cube root of the linear volume");
    assert_eq!(g.nodes[&22].volume_pct, 60);
    assert_eq!(g.nodes[&22].bus.as_deref(), Some("bluetooth"));
    assert_eq!(g.nodes[&20].bus.as_deref(), Some("pci"));
    assert_eq!(g.default_output().map(|n| n.id), Some(22));
    assert_eq!(g.default_input().map(|n| n.id), Some(21));
    assert_eq!(g.target(30).map(|n| n.id), Some(22));
    assert_eq!(g.nodes[&30].kind, Kind::Playing);
    assert_eq!(g.nodes[&30].description, "Browser");

    // A change of one parameter keeps the rest; an object printed with no info is gone.
    objects.apply(&json!([{ "id": 22, "info": { "params": { "Props": [ { "channelVolumes": [0.512, 0.512], "mute": true } ] } } }]));
    objects.apply(&json!([{ "id": 30, "info": null }]));
    let g = objects.graph();
    assert_eq!((g.nodes[&22].volume_pct, g.nodes[&22].mute, g.nodes[&22].description.as_str()), (80, true, "Headset"));
    assert!(!g.nodes.contains_key(&30));
}

#[test]
fn a_metadata_change_is_printed_alone_and_keeps_the_rest() {
    let mut objects = Objects::default();
    objects.apply(&laptop());
    // As pw-dump prints it: the default metadata with the one entry that changed.
    let changed = |key: &str, value: Value| {
        json!([{ "id": 40, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
                 "metadata": [ { "subject": 0, "key": key, "value": value } ] }])
    };
    objects.apply(&changed("default.configured.audio.sink", json!({ "name": "headset" })));
    let g = objects.graph();
    assert_eq!((g.default_output().map(|n| n.id), g.default_input().map(|n| n.id)), (Some(22), Some(21)));
    objects.apply(&changed("default.audio.sink", json!({ "name": "speakers" })));
    assert_eq!(objects.graph().default_output().map(|n| n.id), Some(20));
    // A deletion printed with no entry changes nothing; an entry with no value goes.
    objects.apply(&json!([{ "id": 40, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" }, "metadata": [] }]));
    assert_eq!(objects.graph().default_output().map(|n| n.id), Some(20));
    objects.apply(&changed("default.audio.sink", Value::Null));
    assert_eq!(objects.graph().default_output(), None);
}

#[tokio::test]
async fn the_status_is_counted_once_per_change_and_a_late_client_joins_it() {
    let (fake, core) = (Fake::new(), Core::default());
    let a = Audio::new(Ok(fake.clone()));
    a.refresh(&core).await.unwrap();
    assert!(core.take().is_empty(), "the first reading reports nothing and emits nothing");
    handle(&core, &question(STATUS), &a).await.unwrap();
    let s = json_of(&core.take()[0], 1);
    assert_eq!(s["sequence"], 1);
    assert_eq!((s["output"]["id"].clone(), s["output"]["volume_pct"].clone()), (json!(22), json!(60)));
    assert_eq!(s["input"]["description"], "Microphone");

    // Nothing changed: nothing counted.
    a.refresh(&core).await.unwrap();
    handle(&core, &question(STATUS), &a).await.unwrap();
    assert_eq!(json_of(&core.take()[0], 1)["sequence"], 1);

    // A client subscribes, then asks: the stream's first sample and the answer are one record.
    a.activated(&core, STATUS).await.unwrap();
    handle(&core, &question(STATUS), &a).await.unwrap();
    let said = core.take();
    assert_eq!(json_of(&said[0], 2), json_of(&said[1], 1));

    // The default moves to the speakers: counted, and emitted while the stream flows.
    fake.print(json!([defaults("speakers", "mic")]));
    a.refresh(&core).await.unwrap();
    let next = json_of(&core.take()[0], 2);
    assert_eq!((next["sequence"].clone(), next["output"]["description"].clone()), (json!(2), json!("Speakers")));
}

#[tokio::test]
async fn an_output_coming_or_going_is_reported_and_the_pictures_flow_whole() {
    let (fake, core) = (Fake::new(), Core::default());
    let a = Audio::new(Ok(fake.clone()));
    a.refresh(&core).await.unwrap();
    a.activated(&core, DEVICES).await.unwrap();
    a.activated(&core, STREAMS_OF).await.unwrap();
    let said = core.take();
    let devices = json_of(&said[0], 2);
    assert_eq!(devices["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(devices["outputs"][1]["default"], true);
    let streams = json_of(&said[1], 2);
    assert_eq!((streams["playing"][0]["application"].clone(), streams["playing"][0]["output"].clone()), (json!("Browser"), json!(22)));

    // The headset goes: reported, and the default falls to nobody until PipeWire names another.
    fake.print(json!([{ "id": 22, "info": null }, { "id": 50, "info": null }]));
    a.refresh(&core).await.unwrap();
    let said = core.take();
    assert!(said.contains(&format!("report {DEVICE_REMOVED} 10: output Headset went")), "{said:?}");
    assert!(said.iter().any(|l| l.starts_with(&format!("emit {DEVICES} "))), "{said:?}");

    // A USB card comes.
    fake.print(json!([node(23, "Audio/Sink", "usb", json!({ "node.description": "USB Card" }), &[1.0, 1.0], false)]));
    a.refresh(&core).await.unwrap();
    assert!(core.take().contains(&format!("report {DEVICE_ADDED} 10: output USB Card came")));

    // Nothing changed: the pictures are not emitted again.
    a.refresh(&core).await.unwrap();
    assert!(core.take().is_empty());
}

#[tokio::test]
async fn commands_reach_pipewire_and_what_does_not_fit_is_refused() {
    let (fake, core) = (Fake::new(), Core::default());
    let a = Audio::new(Ok(fake.clone()));
    handle(&core, &command(VOLUME_SET, json!({ "id": 22, "volume_pct": 75 })), &a).await.unwrap();
    handle(&core, &command(MUTE_SET, json!({ "id": 30, "mute": true })), &a).await.unwrap();
    handle(&core, &command(DEFAULT_SET, json!({ "id": 20 })), &a).await.unwrap();
    handle(&core, &command(STREAM_MOVE, json!({ "id": 30, "to": 20 })), &a).await.unwrap();
    assert_eq!(fake.asked(), vec!["volume 22 75", "mute 30 true", "default 20", "move 30 speakers"]);
    assert_eq!(
        core.take(),
        vec![
            "ack Done: Headset is at 75%",
            "ack Done: Browser is muted",
            "ack Done: Speakers is the default output",
            "ack Done: Browser is on Speakers",
        ]
    );

    handle(&core, &command(VOLUME_SET, json!({ "id": 22, "volume_pct": 151 })), &a).await.unwrap();
    handle(&core, &command(VOLUME_SET, json!({ "id": 99, "volume_pct": 10 })), &a).await.unwrap();
    handle(&core, &command(DEFAULT_SET, json!({ "id": 30 })), &a).await.unwrap();
    handle(&core, &command(STREAM_MOVE, json!({ "id": 30, "to": 21 })), &a).await.unwrap();
    handle(&core, &command(MUTE_SET, json!({ "id": 22 })), &a).await.unwrap();
    assert!(fake.asked().is_empty(), "nothing that does not fit reaches PipeWire");
    let said = core.take();
    assert!(said[0].starts_with("ack Failed: invalid: a volume is at most 150%"), "{said:?}");
    assert!(said[1].starts_with("ack Failed: not_found: there is no"), "{said:?}");
    assert!(said[2].starts_with("ack Failed: not_found: there is no output or input of id 30"), "{said:?}");
    assert!(said[3].starts_with("ack Failed: not_found: there is no output of id 21"), "{said:?}");
    assert!(said[4].starts_with("ack Failed: invalid: the payload is not what audio.mute.set takes"), "{said:?}");
}

#[tokio::test]
async fn without_pipewire_every_question_and_command_is_failed_with_the_reason() {
    let core = Core::default();
    let a = Audio::new(Err(AudioError::Unavailable("pw-dump is not installed".into())));
    a.refresh(&core).await.unwrap();
    handle(&core, &question(STATUS), &a).await.unwrap();
    handle(&core, &command(MUTE_SET, json!({ "id": 1, "mute": true })), &a).await.unwrap();
    assert_eq!(
        core.take(),
        vec!["fail unavailable: pw-dump is not installed", "ack Failed: unavailable: pw-dump is not installed"]
    );
}
