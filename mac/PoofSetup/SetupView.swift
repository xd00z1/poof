import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SetupView: View {
    @StateObject private var model = SetupModel()

    @State private var appleID = ""
    @State private var password = ""
    @State private var revoke = false
    @State private var code = ""
    @State private var customIPA: URL?

    private var ipa: URL? { customIPA ?? model.bundledIPA }
    private var hasDevice: Bool { !model.devices.isEmpty }
    private var canInstall: Bool {
        hasDevice && !model.status.running && ipa != nil && appleID.contains("@") && !password.isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            deviceRow
            account
            actions
            Divider()
            progress
        }
        .padding(24)
        .onAppear { model.startPolling() }
        .sheet(isPresented: .constant(model.status.needsCode)) { twoFactorSheet }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Poof Setup").font(.largeTitle.bold())
            Text("One time, with your iPhone plugged in. Afterwards the phone spoofs on its own, on Wi-Fi or cellular.")
                .foregroundStyle(.secondary)
        }
    }

    private var deviceRow: some View {
        HStack(spacing: 8) {
            Image(systemName: hasDevice ? "iphone.gen3.radiowaves.left.and.right" : "iphone.slash")
                .foregroundStyle(hasDevice ? .green : .secondary)
            Text(hasDevice ? "iPhone connected" : "Plug in your iPhone with a cable and unlock it")
            if let udid = model.devices.first {
                Text(udid).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
            }
        }
    }

    private var account: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                TextField("Apple ID email", text: $appleID)
                    .textContentType(.username)
                SecureField("Password", text: $password)
                    .textContentType(.password)
                Toggle("If the certificate limit is hit, revoke old development certificates", isOn: $revoke)
                    .font(.caption)
                Text("Used only to sign Poof for your phone, the same way Xcode, AltServer and Sideloadly do. A free Apple ID's signature lasts 7 days; plug in and press Install again to refresh. Your password is not stored.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack {
                    Text(ipa.map { "App: \($0.lastPathComponent)" } ?? "No Poof.ipa bundled")
                        .font(.caption)
                        .foregroundStyle(ipa == nil ? .red : .secondary)
                    Spacer()
                    Button("Choose IPA…", action: chooseIPA).controlSize(.small)
                }
            }
            .padding(6)
        }
    }

    private var actions: some View {
        HStack {
            Button {
                guard let ipa else { return }
                model.install(appleID: appleID, password: password, ipa: ipa, revoke: revoke)
            } label: {
                Label("Install Poof", systemImage: "arrow.down.app")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(!canInstall)

            Button("Pair only") { model.pairOnly() }
                .controlSize(.large)
                .disabled(!hasDevice || model.status.running)
                .help("Poof is already installed; just create and copy the pairing file.")
        }
    }

    @ViewBuilder
    private var progress: some View {
        if model.status.running {
            HStack { ProgressView().controlSize(.small); Text("Working… keep the iPhone plugged in and unlocked.") }
        }
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(model.status.log.enumerated()), id: \.offset) { _, line in
                    Text("• \(line)").font(.callout).textSelection(.enabled)
                }
                if let e = model.status.error ?? model.startError {
                    Text(e).foregroundStyle(.red).textSelection(.enabled)
                }
                if let r = model.status.report { result(r) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private func result(_ r: SetupStatus.Report) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(r.appPushError == nil ? "Done. You can unplug." : "Almost done",
                  systemImage: r.appPushError == nil ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
                .font(.headline)
                .foregroundStyle(r.appPushError == nil ? .green : .orange)
                .padding(.top, 8)
            Text("On the iPhone:").font(.subheadline.bold())
            if r.installed {
                Text("1. Settings › General › VPN & Device Management › trust your Apple ID.")
            }
            if !r.developerMode {
                Text("2. Settings › Privacy & Security › Developer Mode › On (the phone restarts).")
            }
            Text("3. Install LocalDevVPN from the App Store and turn it on.")
            Text("4. Open Poof › Setup › download the developer disk image. Then tap the map.")
            if let e = r.appPushError {
                Text("The pairing file could not be copied into Poof (\(e)). AirDrop it from \(model.pairingOut.path).")
                    .foregroundStyle(.orange)
                    .textSelection(.enabled)
            }
        }
        .font(.callout)
    }

    private var twoFactorSheet: some View {
        VStack(spacing: 14) {
            Text("Two-factor code").font(.title2.bold())
            Text("Enter the 6-digit code Apple sent to your devices.")
                .foregroundStyle(.secondary)
            if let e = model.status.codeError {
                Text(e).foregroundStyle(.red).font(.caption)
            }
            TextField("123456", text: $code)
                .font(.title.monospaced())
                .multilineTextAlignment(.center)
                .frame(width: 180)
                .onSubmit(submitCode)
            HStack {
                Button("Cancel", role: .cancel) {
                    model.submit(code: nil)
                    code = ""
                }
                Button("Continue", action: submitCode)
                    .keyboardShortcut(.defaultAction)
                    .disabled(code.trimmingCharacters(in: .whitespaces).count < 6)
            }
        }
        .padding(28)
    }

    private func submitCode() {
        model.submit(code: code.trimmingCharacters(in: .whitespaces))
        code = ""
    }

    private func chooseIPA() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [UTType(filenameExtension: "ipa") ?? .data]
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK { customIPA = panel.url }
    }
}
