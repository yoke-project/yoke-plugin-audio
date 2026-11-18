//! audio: the host's audio, through PipeWire, as a Yoke Plugin unit.

use std::sync::Arc;

use audio::backend::{AudioBackend, PipeWire};
use audio::plugin::{declaration, handle, Audio};
use tokio::sync::broadcast::error::RecvError;
use yoke_sdk::plugin::{start, Event};

#[tokio::main]
async fn main() {
    // `audio --manifest` prints the Manifest the declaration generates, which the bundle carries.
    if std::env::args().nth(1).as_deref() == Some("--manifest") {
        print!("{}", declaration().manifest());
        return;
    }
    let unit = match start(&declaration()).await {
        Ok(unit) => Arc::new(unit),
        Err(err) => {
            eprintln!("audio: {err}");
            std::process::exit(1);
        }
    };
    // Registered: now finding PipeWire. Without it the unit still runs, says why in its health, and fails
    // every question and command with the reason.
    let (ended_tx, mut ended) = tokio::sync::oneshot::channel();
    let backend = tokio::task::spawn_blocking(move || PipeWire::start(ended_tx))
        .await
        .expect("finding PipeWire does not panic")
        .map(|b| b as Arc<dyn AudioBackend>);
    let (grade, line) = match &backend {
        Ok(_) => (0, "PipeWire followed".to_string()),
        Err(e) => (50, format!("no PipeWire: {e}")),
    };
    if let Err(err) = unit.health(grade, &line).await {
        eprintln!("audio: {err}");
    }
    let mut said = backend.as_ref().ok().map(|b| b.subscribe());
    let audio = Audio::new(backend);
    // The graph as the Plugin finds it: sequence 1, and no occurrence.
    if let Err(err) = audio.refresh(unit.as_ref()).await {
        eprintln!("audio: {err}");
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("audio listens for SIGTERM");
    loop {
        tokio::select! {
            event = unit.next() => match event {
                None | Some(Event::Ended(_)) => return,
                Some(Event::Activated(a)) => {
                    if let Err(err) = audio.activated(unit.as_ref(), &a.stream).await {
                        eprintln!("audio: {err}");
                    }
                }
                Some(Event::Stopped { stream }) => audio.stopped(&stream),
                Some(event) => {
                    let (unit, audio) = (unit.clone(), audio.clone());
                    tokio::spawn(async move {
                        if let Err(err) = handle(unit.as_ref(), &event, &audio).await {
                            eprintln!("audio: {err}");
                        }
                    });
                }
            },
            ev = async { match said.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => match ev {
                // A change, or several missed: the graph is read as it stands either way.
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    if let Err(err) = audio.refresh(unit.as_ref()).await {
                        eprintln!("audio: {err}");
                    }
                }
                Err(RecvError::Closed) => said = None,
            },
            why = &mut ended, if said.is_some() => {
                said = None;
                let why = why.unwrap_or_else(|_| "PipeWire went away".into());
                let _ = unit.health(50, &why).await;
            },
            _ = term.recv() => {
                let _ = unit.close().await;
                while let Some(event) = unit.next().await {
                    if matches!(event, Event::Ended(_)) {
                        break;
                    }
                }
                return;
            }
        }
    }
}
