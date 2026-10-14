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
    var iosVersion: String?
    var transport: String?
    var lastError: String?
    var reconnects: Int?
    var lastAck: UInt64?

    static let stopped = EngineStatus(phase: "stopped")

    var isRunning: Bool { phase != "stopped" }
    var isActive: Bool { phase == "active" }
}

enum Speed: String, CaseIterable, Identifiable {
    case walk, jog, bike, drive

    var id: String { rawValue }
    var metresPerSecond: Double {
        switch self {
        case .walk: 1.4
        case .jog: 2.8
        case .bike: 5.5
        case .drive: 13.4
        }
    }
    var label: String { rawValue.capitalized }
}

/// Swift face of the Rust engine. Every call returns immediately; state comes
/// back through `status`, polled while the app is in the foreground.
@MainActor
final class PoofEngine: ObservableObject {
    @Published private(set) var status = EngineStatus.stopped

    private var timer: Timer?
    private let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    var target: CLLocationCoordinate2D? {
        status.target.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lon) }
    }

    /// Starts the engine if it is not running. Returns a user-facing error.
    @discardableResult
    func ensureStarted(_ store: AppStore) -> String? {
        refresh()
        if status.isRunning { return nil }
        guard store.hasPairingFile else { return "Import a pairing file first (Setup)." }

        let ddi = store.hasDDI ? store.ddiDirectory.path : nil
        let refresh = UInt32(max(0, store.refreshSeconds))
        let rc: Int32
        if let ddi {
            rc = poof_start(store.pairingURL.path, store.deviceIP, ddi, refresh)
        } else {
            rc = poof_start(store.pairingURL.path, store.deviceIP, nil, refresh)
        }
        guard rc == POOF_OK else { return "Engine failed to start (\(rc)). Check the device IP." }

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

    /// Walks from the current spoofed position to `dest`. Without a current
    /// position there is nothing to walk from, so it teleports instead.
    @discardableResult
    func walk(to dest: CLLocationCoordinate2D, speed: Speed, store: AppStore) -> String? {
        guard let from = target else { return teleport(to: dest, store: store) }
        if let e = ensureStarted(store) { return e }

        let route: [String: Any] = [
            "points": [
                ["lat": from.latitude, "lon": from.longitude],
                ["lat": dest.latitude, "lon": dest.longitude],
            ],
            "speed_mps": speed.metresPerSecond,
            "loop": false,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: route),
              let json = String(data: data, encoding: .utf8),
              poof_walk(json) == POOF_OK
        else { return "Could not start the walk." }
        store.lastTarget = dest
        refresh()
        return nil
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
        }
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
