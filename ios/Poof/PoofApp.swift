import SwiftUI

@main
struct PoofApp: App {
    @StateObject private var store = AppStore()
    @StateObject private var engine = PoofEngine()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(store)
                .environmentObject(engine)
                .task {
                    engine.store = store
                    let e = engine
                    KeepAlive.shared.onLocation = { [weak e] loc in
                        Task { @MainActor in e?.checkReportedLocation(loc) }
                    }
                    SigningExpiry.scheduleReminders()
                    resumeIfNeeded()
                }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    // Poll only while visible; the engine itself keeps running.
                    if phase == .active {
                        store.importFromDocuments()
                        engine.startPolling()
                    } else {
                        engine.stopPolling()
                    }
                }
        }
    }

    /// After a reboot (or iOS killing the app) the phone is back on real GPS.
    /// Opening the app reconnects and re-applies the last spoofed location.
    private func resumeIfNeeded() {
        guard store.autoResume, store.isReady, let last = store.lastTarget else { return }
        engine.teleport(to: last, store: store)
    }
}
