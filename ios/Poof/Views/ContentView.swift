import MapKit
import SwiftUI

/// What the Go button does.
enum GoMode: String, CaseIterable, Identifiable {
    case teleport, walk, run, bike, drive

    var id: String { rawValue }
    var travel: TravelMode? {
        switch self {
        case .teleport: nil
        case .walk: .walk
        case .run: .run
        case .bike: .bike
        case .drive: .drive
        }
    }
    var icon: String { travel?.icon ?? "sparkles" }
    var label: String { travel?.label ?? "Teleport" }
}

struct ContentView: View {
    @EnvironmentObject private var store: AppStore
    @EnvironmentObject private var engine: PoofEngine

    @State private var camera: MapCameraPosition = .automatic
    @State private var picked: CLLocationCoordinate2D?
    @State private var search = ""
    @State private var mode: GoMode = .teleport
    @State private var message: String?
    @State private var planning = false
    @State private var showSetup = false
    @State private var showFavorites = false
    @State private var naming = false
    @State private var favoriteName = ""
    @State private var confirmJump = false
    @State private var confirmRealGPS = false

    /// Teleports longer than this ask first: a sudden jump across a country is
    /// what location-checking apps flag.
    private let bigJumpMeters = 50_000.0

    var body: some View {
        NavigationStack {
            ZStack(alignment: .bottom) {
                map
                VStack(spacing: 8) {
                    banners
                    Spacer()
                    panel
                }
            }
            .navigationTitle("Poof")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $search, prompt: "Search a place")
            .onSubmit(of: .search) { Task { await runSearch() } }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showFavorites = true } label: { Image(systemName: "star") }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showSetup = true } label: { Image(systemName: "gearshape") }
                }
            }
            .sheet(isPresented: $showSetup) { SetupView() }
            .sheet(isPresented: $showFavorites) {
                FavoritesView { fav in
                    picked = fav.coordinate
                    camera = .region(.init(center: fav.coordinate, latitudinalMeters: 1500, longitudinalMeters: 1500))
                }
            }
            .alert("Save favorite", isPresented: $naming) {
                TextField("Name", text: $favoriteName)
                Button("Save") {
                    if let p = picked ?? engine.target {
                        store.addFavorite(favoriteName.isEmpty ? "Pin" : favoriteName, p)
                    }
                    favoriteName = ""
                }
                Button("Cancel", role: .cancel) {}
            }
            .confirmationDialog("That's a big jump", isPresented: $confirmJump, titleVisibility: .visible) {
                Button("Drive there instead") {
                    mode = .drive
                    go()
                }
                Button("Teleport anyway", role: .destructive) { teleport(force: true) }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Apps that check location can flag an instant jump this far. Driving there takes longer but looks real.")
            }
            .confirmationDialog("Show your real location?", isPresented: $confirmRealGPS, titleVisibility: .visible) {
                Button("Use real GPS", role: .destructive) {
                    engine.stop(store: store)
                    message = nil
                }
                Button("Keep spoofing", role: .cancel) {}
            } message: {
                Text("Apps will see where you really are from now on.")
            }
            .onAppear { if !store.isReady { showSetup = true } }
        }
    }

    // MARK: Map

    private var map: some View {
        MapReader { proxy in
            Map(position: $camera) {
                if engine.route.count > 1 {
                    MapPolyline(coordinates: engine.route)
                        .stroke(.purple.opacity(0.7), lineWidth: 5)
                }
                if let t = engine.target {
                    Annotation("Spoofed", coordinate: t) {
                        Image(systemName: "location.circle.fill")
                            .font(.title)
                            .foregroundStyle(.white, .purple)
                    }
                }
                if let p = picked {
                    Marker("Here", coordinate: p).tint(.orange)
                }
            }
            .mapControls { MapCompass(); MapScaleView() }
            .onTapGesture { point in
                if let c = proxy.convert(point, from: .local) { picked = c }
            }
        }
        .ignoresSafeArea(edges: .bottom)
    }

    // MARK: Banners

    /// The one most important thing that needs attention, if any.
    @ViewBuilder
    private var banners: some View {
        VStack(spacing: 6) {
            if !store.isReady {
                Banner(color: .blue, icon: "wrench.and.screwdriver",
                       title: "Finish setup",
                       detail: "Run Poof Setup on your Mac with the iPhone plugged in.") { showSetup = true }
            } else if engine.status.isRunning && !engine.vpnUp {
                Banner(color: .red, icon: "exclamationmark.shield.fill",
                       title: "LocalDevVPN is off: your real location may show",
                       detail: "Open LocalDevVPN and tap Connect. Poof reconnects by itself.")
            } else if let e = engine.status.lastError, engine.status.isStuck {
                let d = Diagnosis(e)
                Banner(color: engine.status.phase == "failed" ? .red : .orange,
                       icon: "exclamationmark.triangle.fill",
                       title: d.title, detail: d.fix) { showSetup = true }
            }
            if let days = SigningExpiry.daysLeft, days <= 2 {
                Banner(color: .yellow, icon: "clock.badge.exclamationmark",
                       title: days <= 0 ? "Poof stops opening today" : "Poof stops opening in \(days) day\(days == 1 ? "" : "s")",
                       detail: "Plug into your Mac and press Install Poof in Poof Setup to renew.")
            }
        }
        .padding(.horizontal)
        .padding(.top, 4)
    }

    // MARK: Panel

    private var panel: some View {
        VStack(spacing: 12) {
            StatusPill(status: engine.status)

            if engine.status.walking == true, let left = engine.status.routeRemainingM {
                HStack {
                    Text(progressText(left, engine.status.routeEtaS))
                        .font(.footnote.monospacedDigit())
                    Spacer()
                    Button("Stop here") { engine.holdHere(store: store) }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                }
            }

            if let message {
                Text(message).font(.footnote).foregroundStyle(.red).multilineTextAlignment(.center)
            }

            if let p = picked, let t = engine.target {
                Text(distanceText(from: t, to: p)).font(.caption).foregroundStyle(.secondary)
            } else if picked == nil {
                Text("Tap the map or search to pick a spot.").font(.footnote).foregroundStyle(.secondary)
            }

            Picker("Mode", selection: $mode) {
                ForEach(GoMode.allCases) { m in
                    Image(systemName: m.icon).tag(m).accessibilityLabel(m.label)
                }
            }
            .pickerStyle(.segmented)

            if mode == .drive {
                Picker("Average speed", selection: $store.driveSpeed) {
                    ForEach(DriveSpeed.allCases) { Text($0.label).tag($0) }
                }
                .font(.footnote)
            }

            HStack {
                Button(action: go) {
                    if planning {
                        ProgressView().frame(maxWidth: .infinity)
                    } else {
                        Label(goTitle, systemImage: mode.icon).frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(picked == nil || planning || !store.isReady)

                Button { naming = true } label: { Image(systemName: "star.badge.plus") }
                    .buttonStyle(.bordered)
                    .disabled(picked == nil && engine.target == nil)
            }

            Button(role: .destructive) { confirmRealGPS = true } label: {
                Label("Real GPS", systemImage: "location.slash")
            }
            .disabled(!engine.status.isRunning)
        }
        .padding()
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20))
        .padding()
    }

    private var goTitle: String {
        guard mode != .teleport else { return "Teleport" }
        return engine.target == nil ? "Teleport (start point)" : "\(mode.label) there"
    }

    // MARK: Actions

    private func go() {
        guard let dest = picked else { return }
        message = nil
        // Moving needs a start: the first spot is always a teleport.
        guard let travel = mode.travel, let from = engine.target else {
            teleport(force: false)
            return
        }
        planning = true
        Task {
            defer { planning = false }
            do {
                let planned = try await Router.route(from: from, to: dest, mode: travel)
                let mps: Double
                if travel == .drive, let fixed = store.driveSpeed.mps {
                    mps = fixed
                } else if travel == .drive, planned.expectedTravelTime > 0 {
                    // Apple's ETA already reflects road types and traffic.
                    mps = planned.distance / planned.expectedTravelTime
                } else {
                    mps = travel.defaultMps
                }
                message = engine.travel(planned, mps: max(0.5, mps), store: store)
            } catch {
                message = error.localizedDescription
            }
        }
    }

    private func teleport(force: Bool) {
        guard let dest = picked else { return }
        if !force, let from = engine.target, meters(from, dest) > bigJumpMeters {
            confirmJump = true
            return
        }
        message = engine.teleport(to: dest, store: store)
    }

    // MARK: Text

    private func meters(_ a: CLLocationCoordinate2D, _ b: CLLocationCoordinate2D) -> Double {
        CLLocation(latitude: a.latitude, longitude: a.longitude)
            .distance(from: CLLocation(latitude: b.latitude, longitude: b.longitude))
    }

    private func distanceText(from a: CLLocationCoordinate2D, to b: CLLocationCoordinate2D) -> String {
        let m = meters(a, b)
        return m >= 1000
            ? String(format: "%.1f km from the current spoof (straight line)", m / 1000)
            : String(format: "%.0f m from the current spoof (straight line)", m)
    }

    private func progressText(_ meters: Double, _ eta: Double?) -> String {
        let dist = meters >= 1000 ? String(format: "%.1f km", meters / 1000) : String(format: "%.0f m", meters)
        guard let eta, eta.isFinite else { return "\(dist) left" }
        let mins = Int((eta / 60).rounded(.up))
        return mins >= 60 ? "\(dist) left · \(mins / 60) h \(mins % 60) min" : "\(dist) left · \(mins) min"
    }

    private func runSearch() async {
        let req = MKLocalSearch.Request()
        req.naturalLanguageQuery = search
        guard let item = try? await MKLocalSearch(request: req).start().mapItems.first else {
            message = "No results for “\(search)”."
            return
        }
        let c = item.placemark.coordinate
        picked = c
        message = nil
        camera = .region(.init(center: c, latitudinalMeters: 1500, longitudinalMeters: 1500))
    }
}

struct Banner: View {
    let color: Color
    let icon: String
    let title: String
    let detail: String
    var action: (() -> Void)?

    var body: some View {
        let content = HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon).foregroundStyle(color)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.semibold))
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(color.opacity(0.6)))

        if let action {
            Button(action: action) { content }.buttonStyle(.plain)
        } else {
            content
        }
    }
}

struct StatusPill: View {
    let status: EngineStatus

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(title).font(.subheadline.weight(.semibold))
            if let v = status.iosVersion { Text("iOS \(v)").font(.caption).foregroundStyle(.secondary) }
        }
    }

    private var title: String {
        switch status.phase {
        case "active": status.walking == true ? "Moving" : (status.target == nil ? "Connected" : "Spoofing")
        case "connecting": "Connecting…"
        case "mounting_ddi": "Setting up developer image…"
        case "retrying": "Reconnecting…"
        case "failed": "Needs attention"
        default: "Real GPS"
        }
    }

    private var color: Color {
        switch status.phase {
        case "active": .green
        case "connecting", "mounting_ddi": .yellow
        case "retrying": .orange
        case "failed": .red
        default: .gray
        }
    }
}
