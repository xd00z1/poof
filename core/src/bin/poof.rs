//! Desktop companion to the iOS app.
//!
//! One-time setup, iPhone plugged in over USB:
//!
//!   poof setup [--udid <udid>] [--out <file>]
//!
//! Spoofing from a computer with the same engine the app embeds (useful for
//! testing a pairing file before touching the app):
//!
//!   poof --pairing <file> --ip <device-ip> [--ddi <dir>] [--refresh <secs>]
//!
//! Then type commands on stdin:
//!   set <lat> <lon>
//!   walk <speed_mps> <lat,lon> <lat,lon> [...] [loop]
//!   clear | status | quit

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    time::Duration,
};

use poof_core::{
    engine::{Command, Config, Engine, Phase},
    geo::{Coord, Mover, Route},
};

fn usage() -> ! {
    eprintln!(
        "usage: poof setup [--udid <udid>] [--out <file>]\n       poof --pairing <file> --ip <device-ip> [--ddi <dir>] [--refresh <secs>]"
    );
    std::process::exit(2);
}

fn parse_args() -> Config {
    let mut args = std::env::args().skip(1);
    let (mut pairing, mut ip, mut ddi, mut refresh) = (None, None, None, 30u64);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--pairing" => pairing = Some(PathBuf::from(val())),
            "--ip" => ip = Some(val().parse().unwrap_or_else(|_| usage())),
            "--ddi" => ddi = Some(PathBuf::from(val())),
            "--refresh" => refresh = val().parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    Config {
        pairing_file: pairing.unwrap_or_else(|| usage()),
        device_ip: ip.unwrap_or_else(|| usage()),
        ddi_dir: ddi,
        refresh_secs: refresh,
    }
}

fn parse_coord(s: &str) -> Option<Coord> {
    let (a, b) = s.split_once(',')?;
    Coord::new(a.trim().parse().ok()?, b.trim().parse().ok()?)
}

fn parse_line(line: &str) -> Result<Option<Command>, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(None),
        ["set", lat, lon] => {
            let c = lat
                .parse()
                .ok()
                .zip(lon.parse().ok())
                .and_then(|(a, b)| Coord::new(a, b))
                .ok_or("bad coordinate")?;
            Ok(Some(Command::Set(c)))
        }
        ["walk", speed, rest @ ..] => {
            let repeat = rest.last() == Some(&"loop");
            let pts = &rest[..rest.len() - repeat as usize];
            let points = pts
                .iter()
                .map(|p| parse_coord(p))
                .collect::<Option<Vec<_>>>()
                .ok_or("bad point; use lat,lon")?;
            let route = Route {
                points,
                speed_mps: speed.parse().map_err(|_| "bad speed")?,
                repeat,
            };
            Mover::new(route.clone()).ok_or("need 2+ points and speed > 0")?;
            Ok(Some(Command::Walk(route)))
        }
        ["clear"] => Ok(Some(Command::Clear)),
        ["quit"] | ["exit"] => Ok(Some(Command::Shutdown)),
        ["status"] => Ok(None),
        _ => Err(
            "commands: set <lat> <lon> | walk <mps> <lat,lon>... [loop] | clear | status | quit"
                .into(),
        ),
    }
}

fn setup(rt: &tokio::runtime::Runtime) -> ! {
    let mut args = std::env::args().skip(2);
    let (mut udid, mut out) = (None, PathBuf::from("poof.mobiledevicepairing"));
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--udid" => udid = Some(val()),
            "--out" => out = PathBuf::from(val()),
            _ => usage(),
        }
    }

    let res = rt.block_on(poof_core::setup::run(
        udid.as_deref(),
        &out,
        poof_core::setup::APP_BUNDLE_ID,
        |m| println!("• {m}"),
    ));
    let report = match res {
        Ok(r) => r,
        Err(e) => {
            eprintln!("setup failed: {e}");
            std::process::exit(1);
        }
    };

    println!();
    if !report.developer_mode {
        println!("! Developer Mode is off. Its switch is now visible in");
        println!(
            "  Settings > Privacy & Security > Developer Mode. Turn it on; the phone restarts."
        );
    }
    match &report.app_push_error {
        None => {
            println!("✓ Poof has its pairing file. Open it, connect LocalDevVPN, and you're done.")
        }
        Some(e) => {
            println!("! Could not copy into Poof: {e}");
            println!(
                "  Install Poof, then run setup again, or AirDrop {} to the phone.",
                out.display()
            );
        }
    }
    println!("  The pairing stays valid until you 'Reset Location & Privacy' or erase the phone.");
    std::process::exit(0);
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if std::env::args().nth(1).as_deref() == Some("setup") {
        setup(&rt);
    }
    let cfg = parse_args();
    let engine = Engine::spawn(rt.handle(), cfg);

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match parse_line(&line) {
            Ok(Some(cmd)) => {
                let quit = matches!(cmd, Command::Shutdown);
                engine.send(cmd);
                if quit {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("{e}"),
        }
        let _ = writeln!(out, "{}", serde_json::to_string(&engine.status()).unwrap());
    }

    engine.send(Command::Shutdown);
    // Give the engine a moment to clear the location before exiting.
    for _ in 0..50 {
        if engine.status().phase == Phase::Stopped {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert!(matches!(
            parse_line("set 40.7 -74.0"),
            Ok(Some(Command::Set(_)))
        ));
        assert!(
            matches!(parse_line("walk 1.4 1,1 1,1.001 loop"), Ok(Some(Command::Walk(r))) if r.repeat && r.points.len() == 2)
        );
        assert!(parse_line("set 99 0").is_err());
        assert!(parse_line("walk 1.4 1,1").is_err());
        assert!(matches!(parse_line(""), Ok(None)));
    }
}
