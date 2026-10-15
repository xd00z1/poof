import CoreLocation
import Foundation

struct Favorite: Codable, Identifiable, Equatable {
    var id = UUID()
    var name: String
    var lat: Double
    var lon: Double

    var coordinate: CLLocationCoordinate2D { .init(latitude: lat, longitude: lon) }
}

/// Files and settings: the pairing file, the developer disk image, favorites.
@MainActor
final class AppStore: ObservableObject {
    /// Source of the personalized developer disk image (the same files Xcode
    /// mounts). Override in Setup if this repository moves.
    /// iOS 26.4+ installs the developer disk image as a cryptex; earlier
    /// versions mount the personalized image. Same split as StikDebug.
    static let usesCryptexDDI: Bool = {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        return v.majorVersion > 26 || (v.majorVersion == 26 && v.minorVersion >= 4)
    }()
    static let ddiBase = usesCryptexDDI
        ? "https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main/PersonalizedImages/Xcode_iOS_DDI_Cryptex"
        : "https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main/PersonalizedImages/Xcode_iOS_DDI_Personalized"
    static let ddiFiles = usesCryptexDDI
        ? ["BuildManifest.plist", "Image.dmg", "Image.dmg.trustcache", "Image.dmg.cryptex_info", "Image.dmg.root_hash"]
        : ["BuildManifest.plist", "Image.dmg", "Image.dmg.trustcache"]

    private let defaults = UserDefaults.standard

    @Published var deviceIP: String { didSet { defaults.set(deviceIP, forKey: "deviceIP") } }
    @Published var refreshSeconds: Int { didSet { defaults.set(refreshSeconds, forKey: "refreshSeconds") } }
    @Published var autoResume: Bool { didSet { defaults.set(autoResume, forKey: "autoResume") } }
    @Published var favorites: [Favorite] { didSet { save(favorites, "favorites") } }
    @Published private(set) var hasPairingFile = false
    @Published private(set) var hasDDI = false
    @Published private(set) var ddiProgress: String?

    var lastTarget: CLLocationCoordinate2D? {
        get {
            guard let a = defaults.array(forKey: "lastTarget") as? [Double], a.count == 2 else { return nil }
            return .init(latitude: a[0], longitude: a[1])
        }
        set {
            if let c = newValue {
                defaults.set([c.latitude, c.longitude], forKey: "lastTarget")
            } else {
                defaults.removeObject(forKey: "lastTarget")
            }
        }
    }

    let root: URL
    var pairingURL: URL { root.appendingPathComponent("pairing.plist") }
    var ddiDirectory: URL {
        root.appendingPathComponent(Self.usesCryptexDDI ? "DDI-cryptex" : "DDI", isDirectory: true)
    }

    init() {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        root = support.appendingPathComponent("Poof", isDirectory: true)
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

        deviceIP = defaults.string(forKey: "deviceIP") ?? "10.7.0.1"
        // StikDebug re-sends every 4 s; a dropped session is noticed that fast.
        refreshSeconds = defaults.object(forKey: "refreshSeconds") as? Int ?? 5
        autoResume = defaults.object(forKey: "autoResume") as? Bool ?? true
        favorites = (defaults.data(forKey: "favorites"))
            .flatMap { try? JSONDecoder().decode([Favorite].self, from: $0) } ?? []
        rescan()
        importFromDocuments()
    }

    var isReady: Bool { hasPairingFile }

    /// Changes whenever a new pairing file is imported, so the engine restarts
    /// with it.
    var pairingStamp: String {
        let date = (try? FileManager.default.attributesOfItem(atPath: pairingURL.path))?[.modificationDate] as? Date
        return String(date?.timeIntervalSince1970 ?? 0)
    }

    func rescan() {
        let fm = FileManager.default
        hasPairingFile = fm.fileExists(atPath: pairingURL.path)
        hasDDI = Self.ddiFiles.allSatisfy { fm.fileExists(atPath: ddiDirectory.appendingPathComponent($0).path) }
    }

    /// Copies a pairing file chosen in the Files picker into app storage.
    func importPairing(from url: URL) throws {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let data = try Data(contentsOf: url)
        try Self.validatePairing(data)
        try data.write(to: pairingURL, options: .atomic)
        rescan()
    }

    /// Picks up a pairing file dropped into the app's folder in the Files app
    /// or via Finder file sharing.
    func importFromDocuments() {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let files = (try? FileManager.default.contentsOfDirectory(at: docs, includingPropertiesForKeys: nil)) ?? []
        for f in files where ["plist", "mobiledevicepairing"].contains(f.pathExtension.lowercased()) {
            if let data = try? Data(contentsOf: f), (try? Self.validatePairing(data)) != nil {
                try? data.write(to: pairingURL, options: .atomic)
                try? FileManager.default.removeItem(at: f)
            }
        }
        rescan()
    }

    static func validatePairing(_ data: Data) throws {
        let plist = try PropertyListSerialization.propertyList(from: data, format: nil)
        guard let d = plist as? [String: Any],
              d["HostID"] != nil, d["HostPrivateKey"] != nil, d["DeviceCertificate"] != nil
        else {
            throw NSError(domain: "Poof", code: 1, userInfo: [
                NSLocalizedDescriptionKey: "That is not a pairing file (missing HostID / keys).",
            ])
        }
    }

    func downloadDDI() async throws {
        let fm = FileManager.default
        try fm.createDirectory(at: ddiDirectory, withIntermediateDirectories: true)
        defer { ddiProgress = nil; rescan() }
        for (i, name) in Self.ddiFiles.enumerated() {
            ddiProgress = "Downloading \(name) (\(i + 1)/\(Self.ddiFiles.count))"
            guard let url = URL(string: "\(Self.ddiBase)/\(name)") else { throw URLError(.badURL) }
            let (tmp, response) = try await URLSession.shared.download(from: url)
            guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                throw URLError(.badServerResponse, userInfo: [NSURLErrorFailingURLErrorKey: url])
            }
            let dest = ddiDirectory.appendingPathComponent(name)
            try? fm.removeItem(at: dest)
            try fm.moveItem(at: tmp, to: dest)
        }
    }

    func addFavorite(_ name: String, _ c: CLLocationCoordinate2D) {
        favorites.append(Favorite(name: name, lat: c.latitude, lon: c.longitude))
    }

    private func save<T: Encodable>(_ v: T, _ key: String) {
        defaults.set(try? JSONEncoder().encode(v), forKey: key)
    }
}
