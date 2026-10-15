import AVFoundation
import CoreLocation
import UIKit

/// Keeps the app (and so the engine's connection) running in the background
/// by playing silence through an audio session that mixes with other audio,
/// plus coarse background location updates as a second anchor (StikDebug uses
/// the same pair; a game that grabs the audio session can stop the first).
///
/// This is where the battery cost comes from: the process never suspends.
/// iOS can still kill it under memory pressure, and a reboot always ends it.
final class KeepAlive: NSObject, CLLocationManagerDelegate {
    static let shared = KeepAlive()

    /// Every location iOS reports while running. The engine uses it as a
    /// watchdog: a report far from the spoof means the spoof dropped.
    var onLocation: ((CLLocation) -> Void)?

    private var player: AVAudioPlayer?
    private var observers: [NSObjectProtocol] = []
    private let location: CLLocationManager = {
        let m = CLLocationManager()
        // Coarse: the point is staying resident and noticing big jumps (the
        // spoof dropping), not precise fixes. While the spoof holds still,
        // nothing changes and no updates arrive.
        m.desiredAccuracy = kCLLocationAccuracyKilometer
        m.distanceFilter = 500
        m.pausesLocationUpdatesAutomatically = false
        return m
    }()

    private override init() {
        super.init()
        location.delegate = self
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        DispatchQueue.main.async { self.onLocation?(last) }
    }

    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        if player != nil { startLocation() }
    }

    func start() {
        startLocation()
        guard player == nil else { return }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, options: [.mixWithOthers])
            try session.setActive(true)
            let p = try AVAudioPlayer(data: Self.silentWAV())
            p.numberOfLoops = -1
            p.volume = 0
            p.prepareToPlay()
            p.play()
            player = p
            observe()
        } catch {
            NSLog("Poof keep-alive failed: \(error)")
        }
    }

    func stop() {
        location.stopUpdatingLocation()
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers.removeAll()
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func startLocation() {
        switch location.authorizationStatus {
        case .notDetermined:
            location.requestAlwaysAuthorization()
        case .denied, .restricted:
            return // audio alone still works
        default:
            break
        }
        location.allowsBackgroundLocationUpdates = true
        location.startUpdatingLocation()
    }

    /// Restarts playback if something stopped it. iOS doesn't promise an
    /// interruption-ended notification (a call, Siri, an alarm), so this also
    /// runs on every other chance we get.
    func ensurePlaying() {
        guard let player, !player.isPlaying else { return }
        try? AVAudioSession.sharedInstance().setActive(true)
        player.play()
    }

    private func observe() {
        let nc = NotificationCenter.default
        let on = { [weak self] (name: Notification.Name) in
            self?.observers.append(nc.addObserver(forName: name, object: nil, queue: .main) { _ in
                self?.ensurePlaying()
            })
        }
        on(AVAudioSession.interruptionNotification)
        on(AVAudioSession.routeChangeNotification)
        on(AVAudioSession.silenceSecondaryAudioHintNotification)
        on(UIApplication.didBecomeActiveNotification)
        on(UIApplication.willEnterForegroundNotification)
        observers.append(nc.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.stop()
            self?.start()
        })
    }

    /// One second of 8 kHz mono 16-bit silence as a WAV file.
    private static func silentWAV(sampleRate: UInt32 = 8000, seconds: UInt32 = 1) -> Data {
        let dataSize = sampleRate * seconds * 2
        var d = Data()
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        d.append(contentsOf: Array("RIFF".utf8)); u32(36 + dataSize)
        d.append(contentsOf: Array("WAVE".utf8))
        d.append(contentsOf: Array("fmt ".utf8)); u32(16)
        u16(1) // PCM
        u16(1) // mono
        u32(sampleRate)
        u32(sampleRate * 2) // byte rate
        u16(2) // block align
        u16(16) // bits per sample
        d.append(contentsOf: Array("data".utf8)); u32(dataSize)
        d.append(Data(count: Int(dataSize)))
        return d
    }
}
