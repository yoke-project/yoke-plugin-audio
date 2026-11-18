# audio

A Yoke Plugin that reads and drives the host's audio through PipeWire: the outputs and the inputs, the
applications playing and recording, their volume and mute, the default output and input, and an
application moved to another output.

| | |
| --- | --- |
| questions | `audio.status` (the default output and input, each with its volume and mute, with its sequence), `audio.devices` (every output and input, the default marked), `audio.streams` (every application playing or recording, with the output or input it is on) |
| commands | `audio.volume.set` `{id, volume_pct}`, `audio.mute.set` `{id, mute}`, `audio.default.set` `{id}`, `audio.stream.move` `{id, to}` |
| streams | `audio.status`: every change, none lost; `audio.devices` and `audio.streams`: the whole picture at every change |
| occurrences | `audio.device.added`, `audio.device.removed` (10): an output or an input came or went — a headset connected, a USB card unplugged |

Every payload is JSON and states `schema`; a question takes `{}`. An `id` is PipeWire's id of the node, as
the answers give it. Every occurrence carries its facts as JSON in its detail, which is what an interface
client receives.

**The volume is the one a person reads on a slider**, as PipeWire's own tools show it, in percent: 100 is a
device's full scale, and up to 150 may be set. A command is acknowledged once PipeWire has taken it; what
it changed arrives on the streams.

**An interface that attaches late** subscribes to `audio.status`, then asks it, and drops what the stream
brings at or below the sequence it was answered with.

The Plugin follows the graph with `pw-dump --monitor`, so it is told every change and reads nothing on a
clock, and acts with `wpctl` and `pw-metadata`: `pipewire` and `wireplumber` are what the host must carry.
It declares the need `audio`, and reaches the PipeWire the Core hands it. **A Core run as a service has no
person's session**, so no PipeWire of one: a device whose Core is a service runs a PipeWire its account can
reach, which is the deployment's to arrange.

Not yet: the levels of what plays and what is recorded, for a meter.

Take it into an application with `yoke-compose add plugin https://github.com/yoke-project/yoke-plugin-audio@<tag>`.
Build it with `yoke-compose build`, and try it from `try/`.

Licensed under the Apache License 2.0.
