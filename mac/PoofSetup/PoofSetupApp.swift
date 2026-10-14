import AppKit
import SwiftUI

@main
struct PoofSetupApp: App {
    var body: some Scene {
        WindowGroup("Poof Setup") {
            SetupView()
                .frame(minWidth: 460, minHeight: 600)
        }
        .windowResizability(.contentSize)
    }
}

/// Mirror of `setup_ffi::State` in poof-core.
struct SetupStatus: Decodable {
    struct Report: Decodable {
        var udid: String
        var iosVersion: String
        var developerMode: Bool
        var installed: Bool
        var appPushError: String?
    }

    struct CodePrompt: Decodable {
        struct Phone: Decodable {
            var id: UInt32
            var lastTwo: String
        }

        var lastError: String?
        var sms: Bool
        /// The last method failed: pick one (devices / SMS) before a code.
        var unknown: Bool
        var numbers: [Phone]
    }

    var running = false
    var log: [String] = []
    var needsCode = false
    var codePrompt: CodePrompt?
    var report: Report?
    var error: String?
}

@MainActor
final class SetupModel: ObservableObject {
    @Published var status = SetupStatus()
    @Published var devices: [String] = []
    @Published var startError: String?

    private var timer: Timer?
    private var tick = 0
    private let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    /// Poof.ipa shipped inside this app.
    var bundledIPA: URL? { Bundle.main.url(forResource: "Poof", withExtension: "ipa") }

    var pairingOut: URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Poof", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("poof.mobiledevicepairing")
    }

    func startPolling() {
        guard timer == nil else { return }
        refreshDevices()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.poll() }
        }
    }

    private func poll() {
        if let p = poof_setup_status_json() {
            defer { poof_string_free(p) }
            if let s = try? decoder.decode(SetupStatus.self, from: Data(String(cString: p).utf8)) {
                status = s
            }
        }
        tick += 1
        // Check for a plugged-in iPhone every 2 s while idle.
        if tick % 4 == 0, !status.running { refreshDevices() }
    }

    private var checkingDevices = false

    /// The usbmuxd query can block for up to 2 s; keep it off the main thread.
    private func refreshDevices() {
        guard !checkingDevices else { return }
        checkingDevices = true
        Task.detached {
            var found: [String] = []
            if let p = poof_usb_devices_json() {
                found = (try? JSONDecoder().decode([String].self, from: Data(String(cString: p).utf8))) ?? []
                poof_string_free(p)
            }
            await MainActor.run {
                self.devices = found
                self.checkingDevices = false
            }
        }
    }

    func cancel() {
        _ = poof_setup_cancel()
    }

    func install(appleID: String, password: String, ipa: URL, revoke: Bool, anisette: String?) {
        let rc: Int32
        if let anisette, !anisette.isEmpty {
            rc = poof_setup_start(appleID, password, ipa.path, pairingOut.path, revoke, anisette)
        } else {
            rc = poof_setup_start(appleID, password, ipa.path, pairingOut.path, revoke, nil)
        }
        startError = rc == POOF_OK ? nil : "Could not start (\(rc))."
    }

    func pairOnly() {
        let rc = poof_setup_start(nil, nil, nil, pairingOut.path, false, nil)
        startError = rc == POOF_OK ? nil : "Could not start (\(rc))."
    }

    func submit(code: String?) {
        if let code {
            _ = poof_setup_submit_code(code)
        } else {
            _ = poof_setup_submit_code(nil)
        }
    }
}
