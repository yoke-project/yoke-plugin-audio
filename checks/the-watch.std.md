# The watch

| | |
| --- | --- |
| **Feature** | this Plugin against what Yoke last published |
| **Planning item** | yoke-project/yoke-plugin-audio#1 |

## yoke-plugin-audio:the-watch.01 — built and taken by the newest release of Yoke, the Plugin runs under its Core and answers its status

| Field | Value |
| --- | --- |
| **Cites** | specs/95.40 |
| **Level** | L3 |
| **Method** | check |
| **Not applicable in** | — |
| **Label** | blocking |
| **Precondition** | the newest published programs of Yoke; a PipeWire of the run's own where one is installed, or none |
| **Action** | build the Plugin, take it into `try/`, run it, grant `audio.status` and ask it |
| **Expected** | the instance comes up; the question is answered with a sequence where PipeWire runs, and failed as `unavailable` where none does |
