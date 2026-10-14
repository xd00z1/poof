import AVFoundation

/// Keeps the app (and so the engine's connection) running in the background
/// by playing silence through an audio session that mixes with other audio.
///
/// This is where the battery cost comes from: the process never suspends.
/// iOS can still kill it under memory pressure, and a reboot always ends it.
final class KeepAlive {
    static let shared = KeepAlive()

    private var player: AVAudioPlayer?
    private var observers: [NSObjectProtocol] = []

    private init() {}

    func start() {
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
        for o in observers { NotificationCenter.default.removeObserver(o) }
        observers.removeAll()
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func observe() {
        let nc = NotificationCenter.default
        // A phone call or Siri pauses us; resume when it ends.
        observers.append(nc.addObserver(
            forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
        ) { [weak self] note in
            guard let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  AVAudioSession.InterruptionType(rawValue: raw) == .ended
            else { return }
            try? AVAudioSession.sharedInstance().setActive(true)
            self?.player?.play()
        })
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
