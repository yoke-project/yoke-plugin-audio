# Trying audio

This directory is an application project that uses the Plugin in `..`: `yoke-compose init` made it and
`add plugin .. --yes` took the Plugin into it. Nothing here is the Plugin, and taking the Plugin into
another application never takes this directory.

| Path | What it is |
| --- | --- |
| `yoke.yaml` | the application: the unit `audio` and the channel `panel`, a local socket for one client |
| `follow/` | an interface client: it asks every question, follows every stream and every event of the unit, and prints each as one line |
| `run.sh` | runs the application on a local Core, grants what the Plugin declares, and follows the unit |

A panel for it is a shell's: `yk-qshell`'s audio panel speaks to this Plugin through the contract.

The programs of Yoke are installed from the module proxy, the newest release or a tag:

```sh
for p in yoke yoke-compose yoke-core yoke-guardian yoke-auth; do
  go install "github.com/yoke-project/yoke/cmd/$p@latest"
done
```

```sh
./run.sh                    # one line per answer, frame and event, until interrupted
./run.sh --read-only        # the questions, the streams and the occurrences, and no command
./run.sh --seconds 10
```

Connect a headset while it runs to see `audio.devices` move and `audio.device.added` reported. A command
goes from another terminal, naming the instance `run.sh` prints:

    echo '{"id":<id>,"volume_pct":40}' | yoke unit command audio audio.volume.set --input - --instance <instance>
