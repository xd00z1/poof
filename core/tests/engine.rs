//! Engine behaviour without a device: failure handling, retry, and command
//! intake while a connection attempt is stuck.

use std::{path::PathBuf, time::Duration};

use poof_core::{
    engine::{Command, Config, Engine, Phase},
    geo::Coord,
};

async fn wait_for(engine: &Engine, pred: impl Fn(&poof_core::engine::Status) -> bool) -> bool {
    for _ in 0..100 {
        if pred(&engine.status()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn cfg(pairing: PathBuf) -> Config {
    Config {
        pairing_file: pairing,
        device_ip: "127.0.0.1".parse().unwrap(),
        ddi_dir: None,
        refresh_secs: 30,
    }
}

#[tokio::test]
async fn bad_pairing_file_fails_until_asked_again() {
    let engine = Engine::spawn(
        &tokio::runtime::Handle::current(),
        cfg("/nonexistent.plist".into()),
    );
    // Unfixable by waiting: no timer retries, it parks in Failed.
    assert!(wait_for(&engine, |s| s.phase == Phase::Failed).await);
    let st = engine.status();
    assert!(st.last_error.unwrap().contains("pairing file"));
    assert_eq!(st.reconnects, 1);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(engine.status().reconnects, 1, "retried on a timer");

    // A new target is the cue to try again.
    let paris = Coord::new(48.8584, 2.2945).unwrap();
    engine.send(Command::Set(paris));
    assert!(wait_for(&engine, |s| s.target == Some(paris) && s.reconnects == 2).await);
    assert_eq!(engine.status().phase, Phase::Failed);

    engine.send(Command::Shutdown);
    assert!(wait_for(&engine, |s| s.phase == Phase::Stopped).await);
    assert!(wait_for(&engine, |_| engine.is_finished()).await);
}

#[tokio::test]
async fn commands_apply_while_connect_hangs() {
    // A "device" that accepts the lockdown connection and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:62078").await;
    let Ok(listener) = listener else {
        eprintln!("port 62078 busy; skipping");
        return;
    };
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });

    // A syntactically valid pairing file so the engine gets as far as lockdown.
    let dir = std::env::temp_dir().join(format!("poof-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pairing = dir.join("pairing.plist");
    std::fs::write(&pairing, include_bytes!("fixtures/pairing.plist")).unwrap();

    let engine = Engine::spawn(&tokio::runtime::Handle::current(), cfg(pairing));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(engine.status().phase, Phase::Connecting);

    let c = Coord::new(35.6586, 139.7454).unwrap();
    engine.send(Command::Set(c));
    assert!(
        wait_for(&engine, |s| s.target == Some(c)).await,
        "target not applied during connect"
    );

    engine.send(Command::Shutdown);
    assert!(wait_for(&engine, |s| s.phase == Phase::Stopped).await);
}
