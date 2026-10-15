import SwiftUI
import UniformTypeIdentifiers

struct SetupView: View {
    @EnvironmentObject private var store: AppStore
    @EnvironmentObject private var engine: PoofEngine
    @Environment(\.dismiss) private var dismiss

    @State private var importing = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Install a loopback VPN such as LocalDevVPN from the App Store and turn it on. It routes Poof's traffic back into this iPhone so it can reach Apple's developer services without a computer.")
                        .font(.footnote)
                    LabeledContent("Device address") {
                        TextField("10.7.0.1", text: $store.deviceIP)
                            .multilineTextAlignment(.trailing)
                            .keyboardType(.numbersAndPunctuation)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                    }
                } header: { step(1, "Loopback VPN", done: nil) }

                Section {
                    Text("Generate it once on a computer with idevice_pair (or jitterbugpair) while the iPhone is plugged in, then AirDrop it here or drop it into Poof's folder in the Files app.")
                        .font(.footnote)
                    Button(store.hasPairingFile ? "Replace pairing file" : "Import pairing file") { importing = true }
                } header: { step(2, "Pairing file", done: store.hasPairingFile) }

                Section {
                    Text("Settings › Privacy & Security › Developer Mode. Needs a restart the first time.")
                        .font(.footnote)
                } header: { step(3, "Developer Mode", done: nil) }

                Section {
                    Text("Apple's developer disk image unmounts on every reboot. Poof re-mounts it on connect from these files (about 20 MB). Not needed if something else already mounted it since the last restart.")
                        .font(.footnote)
                    if let p = store.ddiProgress {
                        HStack { ProgressView(); Text(p).font(.footnote) }
                    } else {
                        Button(store.hasDDI ? "Re-download" : "Download") {
                            Task {
                                do { try await store.downloadDDI() } catch { self.error = error.localizedDescription }
                            }
                        }
                    }
                    Text(AppStore.usesCryptexDDI ? "Cryptex image (iOS 26.4+)" : "Personalized image")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { step(4, "Developer disk image", done: store.hasDDI) }

                Section {
                    Stepper(value: $store.refreshSeconds, in: 1...10, step: 1) {
                        LabeledContent("Re-send every", value: store.refreshSeconds == 0 ? "off" : "\(store.refreshSeconds) s")
                    }
                    Toggle("Restore last spot on launch", isOn: $store.autoResume)
                    Text("If the connection ever drops, iOS shows your real location until Poof notices and reconnects. A short interval keeps that gap to a second or two; the battery cost is small. Changes apply the next time Poof isn't spoofing.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                } header: { Text("Keep-alive") }

                if let error {
                    Section { Text(error).foregroundStyle(.red).font(.footnote) }
                }
            }
            .navigationTitle("Setup")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.propertyList, .data, .item]) { result in
                do {
                    try store.importPairing(from: result.get())
                    error = nil
                } catch {
                    self.error = error.localizedDescription
                }
            }
        }
    }

    private func step(_ n: Int, _ title: String, done: Bool?) -> some View {
        HStack {
            Text("\(n). \(title)")
            Spacer()
            if let done {
                Image(systemName: done ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(done ? .green : .secondary)
            }
        }
    }
}
