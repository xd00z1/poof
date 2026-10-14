//! The keep-alive engine.
//!
//! On iOS 17+ a simulated location only lasts as long as the instruments
//! connection that set it. The engine therefore owns that connection for as
//! long as a spoof is wanted: it connects, applies the target, re-sends it on a
//! timer (which doubles as a liveness probe), answers the lockdown heartbeat,
//! and reconnects with backoff whenever anything drops.

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use idevice::{
    IdeviceError, IdeviceService, ReadWrite,
    core_device_proxy::CoreDeviceProxy,
    dvt::{location_simulation::LocationSimulationClient, remote_server::RemoteServerClient},
    heartbeat::HeartbeatClient,
    lockdown::LockdownClient,
    mobile_image_mounter::ImageMounter,
    pairing_file::PairingFile,
    provider::TcpProvider,
    rsd::RsdHandshake,
    simulate_location::LocationSimulationService,
    tcp::handle::AdapterHandle,
};
use serde::Serialize;
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::geo::{Coord, Mover, Route};

/// Any single request to the device that takes longer than this is treated as
/// a dead connection.
const OP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How often a walked route reports a new position.
const ROUTE_STEP: Duration = Duration::from_secs(1);
/// Upload (~16 MB through the VPN) plus Apple's TSS signing round trip.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(300);

/// Why a session ended. `fatal` errors are not retried on a timer.
#[derive(Debug)]
struct Fail {
    msg: String,
    fatal: bool,
}

impl Fail {
    fn fatal(msg: impl Into<String>) -> Self {
        Self {
            msg: msg.into(),
            fatal: true,
        }
    }
}

impl From<String> for Fail {
    fn from(msg: String) -> Self {
        Self { msg, fatal: false }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub pairing_file: PathBuf,
    /// 10.7.0.1 through a loopback VPN on the phone itself, or the phone's LAN
    /// address from another machine.
    pub device_ip: IpAddr,
    /// Directory holding `Image.dmg`, `Image.dmg.trustcache` and
    /// `BuildManifest.plist` for the personalized developer disk image. Only
    /// needed when the image is not already mounted (it unmounts on reboot).
    pub ddi_dir: Option<PathBuf>,
    /// Seconds between re-sends of a static location. 0 disables re-sending;
    /// the heartbeat still detects most drops.
    pub refresh_secs: u64,
}

#[derive(Debug)]
pub enum Command {
    Set(Coord),
    Walk(Route),
    Clear,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Connecting,
    MountingDdi,
    /// Connected and holding the session; `target` is applied if set.
    Active,
    /// Waiting before the next reconnect attempt.
    Retrying,
    /// Hit an error retrying cannot fix (unsupported iOS, Developer Mode off,
    /// no disk image). Waits for the next command before trying again.
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub target: Option<Coord>,
    pub walking: bool,
    pub ios_version: Option<String>,
    /// "tunnel" (iOS 17+, CoreDeviceProxy + DVT) or "legacy" (com.apple.dt.simulatelocation).
    pub transport: Option<&'static str>,
    pub last_error: Option<String>,
    pub reconnects: u32,
    /// Unix seconds of the last location the device acknowledged.
    pub last_ack: Option<u64>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            phase: Phase::Connecting,
            target: None,
            walking: false,
            ios_version: None,
            transport: None,
            last_error: None,
            reconnects: 0,
            last_ack: None,
        }
    }
}

pub struct Engine {
    tx: mpsc::UnboundedSender<Command>,
    status: Arc<Mutex<Status>>,
    task: JoinHandle<()>,
}

impl Engine {
    pub fn spawn(rt: &tokio::runtime::Handle, cfg: Config) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let task = rt.spawn(run(cfg, rx, status.clone()));
        Self { tx, status, task }
    }

    pub fn send(&self, cmd: Command) -> bool {
        self.tx.send(cmd).is_ok()
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// What the engine is currently trying to show the device.
#[derive(Default)]
struct Intent {
    target: Option<Coord>,
    mover: Option<Mover>,
}

impl Intent {
    fn apply(&mut self, cmd: &Command) {
        match cmd {
            Command::Set(c) => {
                self.target = Some(*c);
                self.mover = None;
            }
            Command::Walk(route) => {
                // `Mover::new` was validated by the caller; fall back to a no-op.
                if let Some(m) = Mover::new(route.clone()) {
                    self.target = Some(m.position());
                    self.mover = Some(m);
                }
            }
            Command::Clear => {
                self.target = None;
                self.mover = None;
            }
            Command::Shutdown => {}
        }
    }
}

enum Exit {
    Shutdown,
}

type Shared = Arc<Mutex<Status>>;

fn update(status: &Shared, f: impl FnOnce(&mut Status)) {
    f(&mut status.lock().unwrap());
}

fn sync_intent(status: &Shared, intent: &Intent) {
    update(status, |s| {
        s.target = intent.target;
        s.walking = intent.mover.is_some();
    });
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn run(cfg: Config, mut rx: mpsc::UnboundedReceiver<Command>, status: Shared) {
    let mut intent = Intent::default();
    let mut backoff = Duration::from_secs(1);

    loop {
        update(&status, |s| s.phase = Phase::Connecting);
        match session(&cfg, &mut rx, &mut intent, &status).await {
            Ok(Exit::Shutdown) => break,
            Err(Fail { msg, fatal }) => {
                tracing::warn!("session ended: {msg}");
                let was_active = status.lock().unwrap().phase == Phase::Active;
                if was_active {
                    backoff = Duration::from_secs(1);
                }
                update(&status, |s| {
                    s.last_error = Some(msg);
                    s.reconnects += 1;
                    s.phase = if fatal {
                        Phase::Failed
                    } else {
                        Phase::Retrying
                    };
                    s.transport = None;
                });
                if fatal {
                    // Retrying on a timer would fail the same way; wait for
                    // the user to fix things and ask again.
                    backoff = Duration::from_secs(24 * 3600);
                }
            }
        }

        // Keep taking commands while waiting so the UI never blocks on a
        // dead device; a new target cuts the wait short.
        let wait = sleep(backoff);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                cmd = rx.recv() => match cmd {
                    None | Some(Command::Shutdown) => {
                        update(&status, |s| s.phase = Phase::Stopped);
                        return;
                    }
                    Some(cmd) => {
                        intent.apply(&cmd);
                        sync_intent(&status, &intent);
                        if intent.target.is_some() {
                            break;
                        }
                    }
                }
            }
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    update(&status, |s| s.phase = Phase::Stopped);
}

fn err(context: &str) -> impl FnOnce(IdeviceError) -> String + '_ {
    move |e| format!("{context}: {e}")
}

async fn timed<T>(
    what: &str,
    fut: impl Future<Output = Result<T, IdeviceError>>,
) -> Result<T, String> {
    match timeout(OP_TIMEOUT, fut).await {
        Ok(r) => r.map_err(err(what)),
        Err(_) => Err(format!("{what}: timed out")),
    }
}

/// Aborts the wrapped task when dropped, so a session never leaks its
/// heartbeat responder.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Answers lockdownd's heartbeat for as long as the session lives. A failed
/// heartbeat (including the device announcing sleep) only reconnects the
/// heartbeat itself: tearing down the instruments connection over it would
/// revert the location on iOS 17+. A connection that is really dead shows up
/// as a failed location re-send instead.
fn spawn_heartbeat(mut hb: HeartbeatClient, provider: TcpProvider) -> AbortOnDrop {
    let task = tokio::spawn(async move {
        loop {
            let mut interval = 15u64;
            loop {
                match hb.get_marco(interval + 15).await {
                    Ok(next) => interval = next.max(1),
                    Err(e) => {
                        tracing::info!("heartbeat ended: {e}");
                        break;
                    }
                }
                if hb.send_polo().await.is_err() {
                    break;
                }
            }
            hb = loop {
                sleep(Duration::from_secs(5)).await;
                if let Ok(Ok(h)) = timeout(OP_TIMEOUT, HeartbeatClient::connect(&provider)).await {
                    break h;
                }
            };
        }
    });
    AbortOnDrop(task)
}

fn major_version(v: &str) -> u32 {
    v.split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0)
}

/// A connected session, owned so it can outlive the connect future.
struct Link {
    transport: Transport,
    _heartbeat: AbortOnDrop,
}

enum Transport {
    /// The adapter handle owns the software TCP stack the instruments
    /// connection runs over; it must live as long as `instruments`.
    Tunnel {
        _tunnel: AdapterHandle,
        instruments: Instruments,
    },
    Legacy(LocationSimulationService),
}

/// CoreDeviceProxy over lockdown (USB or network) first shipped in iOS 17.4;
/// 17.0–17.3 only offer the QUIC remote-pairing tunnel, which this does not
/// implement.
fn has_lockdown_tunnel(v: &str) -> bool {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    major > 17 || (major == 17 && minor >= 4)
}

async fn session(
    cfg: &Config,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    intent: &mut Intent,
    status: &Shared,
) -> Result<Exit, Fail> {
    // Connecting can take tens of seconds (or hang until OP_TIMEOUT); keep
    // accepting commands meanwhile so the UI reflects the latest target.
    let connecting = connect(cfg, status);
    tokio::pin!(connecting);
    let link = loop {
        tokio::select! {
            r = &mut connecting => break r?,
            cmd = rx.recv() => match cmd {
                None | Some(Command::Shutdown) => return Ok(Exit::Shutdown),
                Some(cmd) => {
                    intent.apply(&cmd);
                    sync_intent(status, intent);
                }
            }
        }
    };

    let Link {
        transport,
        _heartbeat,
    } = link;
    match transport {
        Transport::Tunnel {
            _tunnel,
            mut instruments,
        } => {
            let client = timed(
                "open LocationSimulation channel",
                LocationSimulationClient::new(&mut instruments),
            )
            .await?;
            update(status, |s| s.transport = Some("tunnel"));
            drive(Spoofer::Tunnel(client), cfg, rx, intent, status)
                .await
                .map_err(Fail::from)
        }
        Transport::Legacy(client) => {
            update(status, |s| s.transport = Some("legacy"));
            drive(Spoofer::Legacy(client), cfg, rx, intent, status)
                .await
                .map_err(Fail::from)
        }
    }
}

async fn connect(cfg: &Config, status: &Shared) -> Result<Link, Fail> {
    let pairing_file = PairingFile::read_from_file(&cfg.pairing_file)
        .map_err(|e| Fail::fatal(format!("could not read pairing file: {e}")))?;
    let provider = TcpProvider {
        addr: cfg.device_ip,
        scope_id: None,
        pairing_file,
        label: "poof".into(),
    };

    let mut lockdown = timed("lockdown connect", LockdownClient::connect(&provider)).await?;
    timed(
        "lockdown session",
        lockdown.start_session(&provider.pairing_file),
    )
    .await?;
    let version = timed(
        "read iOS version",
        lockdown.get_value(Some("ProductVersion"), None),
    )
    .await?
    .as_string()
    .unwrap_or_default()
    .to_string();
    update(status, |s| s.ios_version = Some(version.clone()));

    // Without a heartbeat responder lockdownd tears down the services we open.
    let hb = timed("heartbeat connect", HeartbeatClient::connect(&provider)).await?;
    let heartbeat = spawn_heartbeat(
        hb,
        TcpProvider {
            addr: provider.addr,
            scope_id: provider.scope_id,
            pairing_file: provider.pairing_file.clone(),
            label: provider.label.clone(),
        },
    );

    let transport = if major_version(&version) >= 17 {
        if !has_lockdown_tunnel(&version) {
            return Err(Fail::fatal(format!(
                "iOS {version} is not supported; update to iOS 17.4 or later"
            )));
        }
        ensure_ddi(cfg, &provider, &mut lockdown, status).await?;
        drop(lockdown);

        let proxy = timed("CoreDeviceProxy", CoreDeviceProxy::connect(&provider)).await?;
        let rsd_port = proxy.tunnel_info().server_rsd_port;
        let mut tunnel = proxy
            .create_software_tunnel()
            .map_err(err("software tunnel"))?
            .to_async_handle();
        let rsd_stream = match timeout(OP_TIMEOUT, tunnel.connect(rsd_port)).await {
            Ok(r) => r.map_err(|e| format!("RSD connect: {e}"))?,
            Err(_) => return Err(Fail::from("RSD connect: timed out".to_string())),
        };
        let mut rsd = timed("RSD handshake", RsdHandshake::new(rsd_stream)).await?;
        let instruments = timed(
            "instruments connect",
            connect_instruments(&mut rsd, &mut tunnel),
        )
        .await?;
        Transport::Tunnel {
            _tunnel: tunnel,
            instruments,
        }
    } else {
        drop(lockdown);
        Transport::Legacy(
            timed(
                "com.apple.dt.simulatelocation (is the developer disk image mounted?)",
                LocationSimulationService::connect(&provider),
            )
            .await?,
        )
    };

    Ok(Link {
        transport,
        _heartbeat: heartbeat,
    })
}

type Instruments = RemoteServerClient<Box<dyn ReadWrite + 'static>>;

/// Boxing pins the concrete service type; without it rustc's `Send` check on the
/// spawned future trips over the higher-ranked `RsdService` impl.
fn connect_instruments<'a>(
    rsd: &'a mut RsdHandshake,
    handle: &'a mut AdapterHandle,
) -> Pin<Box<dyn Future<Output = Result<Instruments, IdeviceError>> + Send + 'a>> {
    Box::pin(rsd.connect::<Instruments>(handle))
}

/// Mounts the personalized developer disk image if it is not mounted yet. It
/// survives until the next reboot, which is why a reboot needs the app again.
async fn ensure_ddi(
    cfg: &Config,
    provider: &TcpProvider,
    lockdown: &mut LockdownClient,
    status: &Shared,
) -> Result<(), Fail> {
    let mut mounter = timed("image mounter", ImageMounter::connect(provider)).await?;

    // A timed-out request leaves its reply unread on this connection, so any
    // failure here must end the session rather than carry on and misread it.
    if !timed("developer mode", mounter.query_developer_mode_status()).await? {
        return Err(Fail::fatal(
            "Developer Mode is off (Settings > Privacy & Security > Developer Mode)",
        ));
    }

    match timeout(OP_TIMEOUT, mounter.lookup_image("Personalized")).await {
        Ok(Ok(_)) => return Ok(()),
        Ok(Err(IdeviceError::NotFound)) => {} // not mounted
        Ok(Err(e)) => return Err(format!("lookup image: {e}").into()),
        Err(_) => return Err("lookup image: timed out".to_string().into()),
    }

    let dir = cfg.ddi_dir.as_deref().ok_or_else(|| {
        Fail::fatal("developer disk image is not mounted: download it in Poof > Setup")
    })?;
    update(status, |s| s.phase = Phase::MountingDdi);

    let read = |name: &str| -> Result<Vec<u8>, Fail> {
        std::fs::read(dir.join(name)).map_err(|e| {
            Fail::fatal(format!(
                "read {}: {e} (download the disk image again in Setup)",
                Path::new(name).display()
            ))
        })
    };
    let image = read("Image.dmg")?;
    let trust_cache = read("Image.dmg.trustcache")?;
    let manifest = read("BuildManifest.plist")?;

    let chip_id = timed(
        "read UniqueChipID",
        lockdown.get_value(Some("UniqueChipID"), None),
    )
    .await?
    .as_unsigned_integer()
    .ok_or_else(|| "UniqueChipID missing".to_string())?;

    // Upload plus TSS signing is slow, but not unbounded: a stalled link must
    // fall back into the retry loop instead of sitting in MountingDdi.
    match timeout(
        MOUNT_TIMEOUT,
        mounter.mount_personalized(provider, image, trust_cache, &manifest, None, chip_id),
    )
    .await
    {
        Ok(r) => r.map_err(err("mount developer disk image"))?,
        Err(_) => return Err("mount developer disk image: timed out".to_string().into()),
    }
    update(status, |s| s.phase = Phase::Connecting);
    Ok(())
}

enum Spoofer<'a> {
    Tunnel(LocationSimulationClient<'a, Box<dyn ReadWrite>>),
    Legacy(LocationSimulationService),
}

impl Spoofer<'_> {
    async fn set(&mut self, c: Coord) -> Result<(), String> {
        match self {
            Spoofer::Tunnel(s) => timed("set location", s.set(c.lat, c.lon)).await,
            Spoofer::Legacy(s) => {
                let (lat, lon) = (c.lat.to_string(), c.lon.to_string());
                timed("set location", s.set(&lat, &lon)).await
            }
        }
    }

    async fn clear(&mut self) -> Result<(), String> {
        match self {
            Spoofer::Tunnel(s) => timed("clear location", s.clear()).await,
            Spoofer::Legacy(s) => timed("clear location", s.clear()).await,
        }
    }
}

async fn drive(
    mut sp: Spoofer<'_>,
    cfg: &Config,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    intent: &mut Intent,
    status: &Shared,
) -> Result<Exit, String> {
    let ack = |status: &Shared| update(status, |s| s.last_ack = Some(now_unix()));

    if let Some(c) = intent.target {
        sp.set(c).await?;
        ack(status);
    }
    update(status, |s| {
        s.phase = Phase::Active;
        s.last_error = None;
    });

    let refresh = (cfg.refresh_secs > 0).then(|| Duration::from_secs(cfg.refresh_secs));
    loop {
        let wait = if intent.mover.is_some() {
            ROUTE_STEP
        } else if let (Some(r), Some(_)) = (refresh, intent.target) {
            r
        } else {
            Duration::from_secs(24 * 3600)
        };

        tokio::select! {
            cmd = rx.recv() => {
                let cmd = cmd.unwrap_or(Command::Shutdown);
                if let Command::Shutdown = cmd {
                    // Best effort: on iOS 17+ closing the connection reverts anyway,
                    // but older versions keep the spoof until told otherwise.
                    let _ = sp.clear().await;
                    intent.apply(&Command::Clear);
                    sync_intent(status, intent);
                    return Ok(Exit::Shutdown);
                }
                intent.apply(&cmd);
                sync_intent(status, intent);
                match intent.target {
                    Some(c) => sp.set(c).await?,
                    None => sp.clear().await?,
                }
                ack(status);
            }
            _ = sleep(wait) => {
                if let Some(m) = intent.mover.as_mut() {
                    let pos = m.advance(ROUTE_STEP.as_secs_f64());
                    intent.target = Some(pos);
                    if m.finished() {
                        intent.mover = None;
                    }
                    sync_intent(status, intent);
                }
                if let Some(c) = intent.target {
                    sp.set(c).await?;
                    ack(status);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gates() {
        assert_eq!(major_version("16.7.10"), 16);
        assert!(!has_lockdown_tunnel("17.3.1"));
        assert!(has_lockdown_tunnel("17.4"));
        assert!(has_lockdown_tunnel("18.0"));
        assert!(has_lockdown_tunnel("26.1"));
    }
}
