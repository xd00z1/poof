# Poof

Location spoofing on iPhone with no jailbreak and no computer after setup. Poof sends coordinates through Apple's developer location-simulation service, the same one Xcode's "Simulate Location" uses, and keeps the session to that service open, which iOS 17+ requires for the spoof to stay in place.

## How it works

```
Poof.app ──TCP──> 10.7.0.1 (loopback VPN, e.g. LocalDevVPN) ──> lockdownd on the same iPhone
   │                                                   │
   │  pairing file authenticates as a trusted "computer"
   │
   ├─ iOS 17.4+: RPPairing (port 49152) → TLS-PSK tunnel → RSD → com.apple.instruments.dtservicehub
   │           (required on iOS 26.4+; lockdown + CoreDeviceProxy is the fallback for 17.4–26.3)
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

## Setup (once, on a Mac, iPhone plugged in)

1. Download **Poof Setup** for Mac (`Poof-Setup-mac` CI artifact), unzip it, and open it. It isn't notarized, so the first time, right-click › Open, or run `xattr -dr com.apple.quarantine "Poof Setup.app"`.
2. Plug in the iPhone, unlock it, enter your Apple ID and press **Install Poof**:
   - it signs the bundled Poof.ipa with your Apple ID and installs it (via [isideload](https://crates.io/crates/isideload), the engine behind iloader), asking for your 2FA code if Apple wants one;
   - it creates a pairing that belongs to Poof only (tap **Trust** on the phone) and turns on network lockdown access (`EnableWifiDebugging`), so that pairing works away from the Mac and USB;
   - it shows the Developer Mode switch in Settings if Developer Mode is off;
   - it copies the pairing file straight into Poof.
3. On the iPhone:
   - trust your Apple ID under Settings › General › VPN & Device Management;
   - turn on Developer Mode if setup asked you to;
   - install **LocalDevVPN** from the App Store and turn it on;
   - in Poof › Setup, download the developer disk image (once).

After that the Mac is out of the picture. On Wi-Fi or cellular, the traffic loops back inside the phone: Poof → VPN → 10.7.0.1 → the phone's own lockdownd.

**Things that make you come back to the Mac:**
- **Free Apple ID:** the signature expires after **7 days**, and then Poof won't open. Plug in and press Install again to refresh. The pairing survives. A paid developer account lasts a year.
- **Reset Location & Privacy, erasing the phone, or untrusting computers:** run setup again.
- **A reboot doesn't need the Mac.** Turn LocalDevVPN back on and open Poof.

The CLI does the same steps: `poof setup --install Poof.ipa --apple-id you@example.com`.

**Your Apple ID:** the password goes only to Apple. Apple's sign-in also needs "anisette" device headers, which isideload fetches from a public anisette server, the same way SideStore and iloader do. Use a secondary Apple ID if that bothers you. The development certificate and its key are kept in `~/Library/Application Support/Poof/signing` so refreshes reuse them.

## Building

The Rust core needs Apple's SDK to cross-compile, so the IPA builds on macOS:

```sh
cd core && cargo build --release --lib --no-default-features --target aarch64-apple-ios
cd ../ios && xcodegen generate && open Poof.xcodeproj   # sign with your team, run
```

CI (`.github/workflows/build.yml`) runs core tests and clippy on Linux. It also builds an **unsigned** `Poof.ipa` on macOS and uploads it as an artifact. Sign and install that IPA with SideStore, AltStore or your own developer account.

### Desktop driver

`poof` without `setup` runs the same engine from a computer on the phone's network. It's useful for checking a pairing file:

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
| `core/src/setup.rs`, `core/src/setup_ffi.rs` | One-time USB setup: sign + install, pair, network lockdown, Developer Mode, push to app |
| `mac/` | Poof Setup, the Mac app (SwiftUI) with the Install button |
| `core/src/bin/poof.rs` | Desktop CLI (`setup` + engine driver) |
| `ios/` | SwiftUI app (XcodeGen `project.yml`) |

## Limits and risks

- **Detection.** Apps can detect the spoof. They can compare GPS against Wi-Fi, cell or IP location, check `CLLocationSourceInformation.isSimulatedBySoftware` (iOS 15+), or notice that Developer Mode is on. Poof doesn't hide any of this. Accounts tied to location (games, dating, banking geofences) can be flagged or banned. Teleporting long distances makes that likelier, so use **Move**.
- **iOS updates** can change or remove these developer services, or the DDI, and break Poof until the engine catches up.
- **Battery.** The silent-audio keep-alive and periodic re-sends keep the process awake. Expect roughly 10–40% extra drain per day, depending on the re-send interval and how often the connection drops. A re-send interval of 0 saves some of that but detects drops later.
- **The pairing file is a full trust credential for your phone.** Anyone holding it, with network access to the phone, can use the same developer services. Keep it private.
- iOS can still kill the app under memory pressure. When that happens the spoof ends until you open Poof again.
