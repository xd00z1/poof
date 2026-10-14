# Poof

Location spoofing on iPhone with no jailbreak and no computer after setup. Poof sends coordinates through Apple's developer location-simulation service, the same one Xcode's "Simulate Location" uses, and keeps the session to that service open, which iOS 17+ requires for the spoof to stay in place.

## How it works

```
Poof.app ──TCP──> 10.7.0.1 (loopback VPN, e.g. LocalDevVPN) ──> lockdownd on the same iPhone
   │                                                   │
   │  pairing file authenticates as a trusted "computer"
   │
   ├─ iOS 17+: CoreDeviceProxy tunnel → RSD → com.apple.instruments.dtservicehub
   │           → LocationSimulation.simulateLocationWithLatitude:longitude:
   └─ iOS ≤16: com.apple.dt.simulatelocation (legacy; spoof persists without a session)
```

- **iOS 17+ only holds the spoof while the connection is open.** If the session drops, the phone goes back to real GPS. The engine (`core/`, Rust, built on [`idevice`](https://crates.io/crates/idevice)) therefore:
  - answers the lockdown heartbeat (marco/polo) so lockdownd keeps the services open,
  - re-sends the target every *N* seconds (default 30). This also tells it quickly when the connection has died,
  - reconnects with exponential backoff (1 s → 30 s) and re-applies the target,
  - keeps accepting new targets while it connects or retries.
- **Background keep-alive** comes from a silent, mixable audio session (`KeepAlive.swift`). It's the only App Store-API way to stay resident without location updates, and it's where the battery cost comes from, since the process never suspends.
- **Reboots reset everything.** The developer disk image unmounts and the app is gone. Open Poof after a restart: it re-mounts the personalized DDI (downloaded once during setup) and restores the last location.
- **Moving instead of teleporting.** "Move" walks the spoof along a straight line at walking, jog, bike or drive speed, with one position update per second.

## Setup (once per device)

1. **Developer Mode on.** Settings › Privacy & Security › Developer Mode.
2. **Pairing file.** With the iPhone plugged into a computer, generate one with `idevice_pair` (from the idevice project) or `jitterbugpair`, then AirDrop it to the phone or drop it into Poof's folder in Files.
3. **Loopback VPN.** Install LocalDevVPN (or StosVPN) and connect it. Poof talks to `10.7.0.1`, which you can change in Setup.
4. **Developer disk image.** Setup › Download. By default it comes from `doronz88/DeveloperDiskImage`; the source URL can be changed.
5. Tap the map, then **Teleport** or **Move**. **Real GPS** clears the spoof and disconnects.

## Building

The Rust core needs Apple's SDK to cross-compile, so the IPA builds on macOS:

```sh
cd core && cargo build --release --lib --target aarch64-apple-ios
cd ../ios && xcodegen generate && open Poof.xcodeproj   # sign with your team, run
```

CI (`.github/workflows/build.yml`) runs core tests and clippy on Linux. It also builds an **unsigned** `Poof.ipa` on macOS and uploads it as an artifact. Sign and install that IPA with SideStore, AltStore or your own developer account.

### Desktop driver

The same engine also runs from a Mac or Linux box on the phone's network. It's useful for checking a pairing file before you touch the app:

```sh
cd core && cargo run --release -- --pairing pairing.plist --ip <phone-lan-ip> [--ddi ./DDI]
set 48.8584 2.2945
walk 1.4 48.8584,2.2945 48.8606,2.3376
clear
quit
```

## Layout

| Path | What |
|---|---|
| `core/src/engine.rs` | Connect, DDI mount, heartbeat, re-send, reconnect state machine |
| `core/src/geo.rs` | Coordinates, haversine, route movement |
| `core/src/lib.rs`, `core/include/poof.h` | C ABI the app links |
| `core/src/bin/poof.rs` | Desktop CLI |
| `ios/` | SwiftUI app (XcodeGen `project.yml`) |

## Limits and risks

- **Detection.** Apps can detect the spoof. They can compare GPS against Wi-Fi, cell or IP location, check `CLLocationSourceInformation.isSimulatedBySoftware` (iOS 15+), or notice that Developer Mode is on. Poof doesn't hide any of this. Accounts tied to location (games, dating, banking geofences) can be flagged or banned. Teleporting long distances makes that likelier, so use **Move**.
- **iOS updates** can change or remove these developer services, or the DDI, and break Poof until the engine catches up.
- **Battery.** The silent-audio keep-alive and periodic re-sends keep the process awake. Expect roughly 10–40% extra drain per day, depending on the re-send interval and how often the connection drops. A re-send interval of 0 saves some of that but detects drops later.
- **The pairing file is a full trust credential for your phone.** Anyone holding it, with network access to the phone, can use the same developer services. Keep it private.
- iOS can still kill the app under memory pressure. When that happens the spoof ends until you open Poof again.
