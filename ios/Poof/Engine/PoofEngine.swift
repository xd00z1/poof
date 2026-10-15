import CoreLocation
import Foundation

/// Mirror of `engine::Status` in poof-core.
struct EngineStatus: Decodable, Equatable {
    struct Coord: Codable, Equatable {
        let lat: Double
        let lon: Double
    }

    var phase: String
    var target: Coord?
    var walking: Bool?
    var routeRemainingM: Double?
    var routeEtaS: Double?
    var iosVersion: String?
    var transport: String?
    var lastError: String?
    var reconnects: Int?
    var lastAck: UInt64?

    static let stopped = EngineStatus(phase: "stopped")

    var isRunning: Bool { phase != "stopped" }
    /// Stuck until something changes: retrying on a timer, or failed outright.
    var isStuck: Bool { phase == "retrying" || phase == "failed" }
    var isActive: Bool { phase == "active" }
}

/// Swift face of the Rust engine. Every call returns immediately; state comes
/// back through `status`, polled while the app is in the foreground.
@MainActor
final class PoofEngine: ObservableObject {
    @Published private(set) var status = EngineStatus.stopped
    /// The road route being travelled, for drawing on the map.
    @Published private(set) var route: [CLLocationCoordinate2D] = []
    @Published private(set) var vpnUp = VPNStatus.isActive
    /// Set by the app; the watchdog and auto-downloads need it.
    weak var store: AppStore?

    private var timer: Timer?
    /// Settings the running engine was started with.
    private var startedWith: [String]?
    private let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    var target: CLLocationCoordinate2D? {
        status.target.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lon) }
    }

    /// Starts the engine if it is not running, or restarts it when it is
    /// stuck or its settings changed (pairing file, address, disk image,
    /// re-send interval are fixed when the engine starts). Returns a
    /// user-facing error.
    @discardableResult
    func ensureStarted(_ store: AppStore) -> String? {
        refresh()
        guard store.hasPairingFile else { return "Import a pairing file first (Setup)." }

        let ddi = store.hasDDI ? store.ddiDirectory.path : nil
        let refresh = UInt32(max(0, store.refreshSeconds))
        let config = [store.pairingURL.path, store.deviceIP, ddi ?? "", String(refresh), store.pairingStamp]
        if status.isRunning, !status.isStuck, config == startedWith { return nil }
        // Restarting a working session would show the real location for a
        // moment. Changed settings wait until the session is not active.
        if status.isActive, startedWith != nil { return nil }
        if !store.hasDDI { store.downloadDDIInBackground { [weak self] in self?.restartIfStuck() } }
        let rc: Int32
        if let ddi {
            rc = poof_start(store.pairingURL.path, store.deviceIP, ddi, refresh)
        } else {
            rc = poof_start(store.pairingURL.path, store.deviceIP, nil, refresh)
        }
        guard rc == POOF_OK else { return "Engine failed to start (\(rc)). Check the device IP." }
        startedWith = config

        KeepAlive.shared.start()
        self.refresh()
        return nil
    }

    @discardableResult
    func teleport(to c: CLLocationCoordinate2D, store: AppStore) -> String? {
        if let e = ensureStarted(store) { return e }
        guard poof_set_location(c.latitude, c.longitude) == POOF_OK else { return "Invalid coordinate." }
        store.lastTarget = c
        refresh()
        return nil
    }

    /// Travels along `planned` (a road or path route) at `mps`, with natural
    /// speed changes and slowing for turns.
    @discardableResult
    func travel(_ planned: PlannedRoute, mps: Double, store: AppStore) -> String? {
        if let e = ensureStarted(store) { return e }
        let body: [String: Any] = [
            "points": planned.points.map { ["lat": $0.latitude, "lon": $0.longitude] },
            "speed_mps": mps,
            "loop": false,
            "vary": true,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body),
              let json = String(data: data, encoding: .utf8),
              poof_walk(json) == POOF_OK
        else { return "Could not start moving." }
        route = planned.points
        store.lastTarget = planned.points.last
        refresh()
        return nil
    }

    /// Stops moving and stays where the spoof is right now.
    func holdHere(store: AppStore) {
        guard let here = target else { return }
        _ = poof_set_location(here.latitude, here.longitude)
        store.lastTarget = here
        route = []
        refresh()
    }

    /// Called when iOS reports a location. Far from the spoof means it has
    /// dropped: re-apply at once (the engine reconnects if needed).
    func checkReportedLocation(_ loc: CLLocation) {
        refresh()
        guard let t = target, status.isRunning else { return }
        let off = loc.distance(from: CLLocation(latitude: t.latitude, longitude: t.longitude))
        if off > 1_000 {
            _ = poof_set_location(t.latitude, t.longitude)
            if status.isStuck { restartIfStuck() }
        }
    }

    private func restartIfStuck() {
        guard let store, status.isStuck || !status.isRunning, let t = store.lastTarget else { return }
        _ = teleport(to: t, store: store)
    }

    /// Back to real GPS: clears the spoof and drops the connection, so nothing
    /// keeps running in the background.
    func stop(store: AppStore) {
        _ = poof_stop()
        store.lastTarget = nil
        KeepAlive.shared.stop()
        refresh()
    }

    func refresh() {
        guard let ptr = poof_status_json() else { return }
        defer { poof_string_free(ptr) }
        let data = Data(String(cString: ptr).utf8)
        if let s = try? decoder.decode(EngineStatus.self, from: data), s != status {
            status = s
            if s.walking != true, !route.isEmpty { route = [] }
        }
        let vpn = VPNStatus.isActive
        if vpn != vpnUp { vpnUp = vpn }
    }

    func startPolling() {
        guard timer == nil else { return }
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
    }

    func stopPolling() {
        timer?.invalidate()
        timer = nil
    }
}
