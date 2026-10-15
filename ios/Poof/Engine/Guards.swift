import Foundation
import UserNotifications

/// Whether a VPN tunnel is up. iOS lists active tunnel interfaces (utun,
/// ipsec, …) in the scoped proxy settings; LocalDevVPN adds a utun one.
enum VPNStatus {
    static var isActive: Bool {
        guard let settings = CFNetworkCopySystemProxySettings()?.takeRetainedValue() as? [String: Any],
              let scoped = settings["__SCOPED__"] as? [String: Any]
        else { return false }
        return scoped.keys.contains { key in
            ["utun", "ipsec", "tun", "tap", "ppp"].contains { key.hasPrefix($0) }
        }
    }
}

/// When this build's signature runs out. A free Apple ID signs for 7 days;
/// after that iOS refuses to open the app until it is installed again.
enum SigningExpiry {
    static let date: Date? = {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let start = data.range(of: Data("<?xml".utf8)),
              let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex)
        else { return nil }
        let plist = data.subdata(in: start.lowerBound..<end.upperBound)
        let dict = try? PropertyListSerialization.propertyList(from: plist, format: nil) as? [String: Any]
        return dict?["ExpirationDate"] as? Date
    }()

    static var daysLeft: Int? {
        date.map { Int(($0.timeIntervalSinceNow / 86_400).rounded(.down)) }
    }

    /// A reminder a day before, and one a few hours before, the app stops opening.
    static func scheduleReminders() {
        guard let date else { return }
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
            guard granted else { return }
            center.removePendingNotificationRequests(withIdentifiers: ["expiry-1d", "expiry-3h"])
            for (id, before) in [("expiry-1d", 86_400.0), ("expiry-3h", 3 * 3_600.0)] {
                let fire = date.addingTimeInterval(-before)
                guard fire > Date() else { continue }
                let content = UNMutableNotificationContent()
                content.title = "Poof needs a refresh"
                content.body = "Plug into your Mac and press Install Poof in Poof Setup, or Poof will stop opening."
                let comps = Calendar.current.dateComponents([.year, .month, .day, .hour, .minute], from: fire)
                let trigger = UNCalendarNotificationTrigger(dateMatching: comps, repeats: false)
                center.add(UNNotificationRequest(identifier: id, content: content, trigger: trigger))
            }
        }
    }
}

/// Turns engine errors into one plain sentence and the single thing to do.
struct Diagnosis {
    let title: String
    let fix: String

    init(_ raw: String) {
        let e = raw.lowercased()
        if e.contains("localdevvpn") || e.contains("connection refused") || e.contains("network is unreachable")
            || (e.contains("timed out") && (e.contains("rppairing") || e.contains("lockdown connect")))
        {
            title = "Can't reach the phone's developer services"
            fix = "Open LocalDevVPN and tap Connect. Poof reconnects by itself."
        } else if e.contains("rppairing verify") || e.contains("pairing file") {
            title = "The pairing isn't accepted"
            fix = "Plug into your Mac, open Poof Setup and press Pair only."
        } else if e.contains("developer mode") {
            title = "Developer Mode is off"
            fix = "Settings › Privacy & Security › Developer Mode › On. The phone restarts."
        } else if e.contains("not supported") {
            title = "This iOS version isn't supported"
            fix = "Update to iOS 17.4 or later."
        } else if e.contains("disk image") || e.contains("tss") || e.contains("cryptex") || e.contains("ddi") {
            title = "Couldn't set up the developer disk image"
            fix = "It needs internet once after every restart. Check your connection; Poof retries by itself."
        } else {
            title = "Connection dropped"
            fix = "Poof is reconnecting by itself."
        }
    }
}
