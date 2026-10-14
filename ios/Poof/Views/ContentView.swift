import MapKit
import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var store: AppStore
    @EnvironmentObject private var engine: PoofEngine

    @State private var camera: MapCameraPosition = .automatic
    @State private var picked: CLLocationCoordinate2D?
    @State private var search = ""
    @State private var speed: Speed = .walk
    @State private var message: String?
    @State private var showSetup = false
    @State private var showFavorites = false
    @State private var naming = false
    @State private var favoriteName = ""

    var body: some View {
        NavigationStack {
            ZStack(alignment: .bottom) {
                map
                panel
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
            .onAppear { if !store.isReady { showSetup = true } }
        }
    }

    private var map: some View {
        MapReader { proxy in
            Map(position: $camera) {
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

    private var panel: some View {
        VStack(spacing: 12) {
            StatusPill(status: engine.status)

            if let message {
                Text(message).font(.footnote).foregroundStyle(.red)
            }

            if let p = picked {
                Text(String(format: "%.5f, %.5f", p.latitude, p.longitude))
                    .font(.footnote.monospaced())
                    .foregroundStyle(.secondary)
                if let t = engine.target {
                    Text(distanceText(from: t, to: p)).font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Text("Tap the map or search to pick a spot.").font(.footnote).foregroundStyle(.secondary)
            }

            HStack {
                Button {
                    guard let p = picked else { return }
                    message = engine.teleport(to: p, store: store)
                } label: { Label("Teleport", systemImage: "sparkles") }
                    .buttonStyle(.borderedProminent)
                    .disabled(picked == nil)

                Menu {
                    Picker("Speed", selection: $speed) {
                        ForEach(Speed.allCases) { Text($0.label).tag($0) }
                    }
                    Button("Go at \(speed.label.lowercased()) speed") {
                        guard let p = picked else { return }
                        message = engine.walk(to: p, speed: speed, store: store)
                    }
                } label: { Label("Move", systemImage: "figure.walk") }
                    .buttonStyle(.bordered)
                    .disabled(picked == nil)

                Button { naming = true } label: { Image(systemName: "star.badge.plus") }
                    .buttonStyle(.bordered)
                    .disabled(picked == nil && engine.target == nil)
            }

            Button(role: .destructive) {
                engine.stop(store: store)
                message = nil
            } label: { Label("Real GPS", systemImage: "location.slash") }
                .disabled(!engine.status.isRunning)
        }
        .padding()
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20))
        .padding()
    }

    private func distanceText(from a: CLLocationCoordinate2D, to b: CLLocationCoordinate2D) -> String {
        let m = CLLocation(latitude: a.latitude, longitude: a.longitude)
            .distance(from: CLLocation(latitude: b.latitude, longitude: b.longitude))
        let km = m / 1000
        let walkMin = Int(m / Speed.walk.metresPerSecond / 60)
        return km >= 1
            ? String(format: "%.1f km from current spoof · ~%d min walking", km, walkMin)
            : String(format: "%.0f m from current spoof · ~%d min walking", m, walkMin)
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

struct StatusPill: View {
    let status: EngineStatus

    var body: some View {
        VStack(spacing: 4) {
            HStack(spacing: 6) {
                Circle().fill(color).frame(width: 8, height: 8)
                Text(title).font(.subheadline.weight(.semibold))
                if let v = status.iosVersion { Text("iOS \(v)").font(.caption).foregroundStyle(.secondary) }
            }
            if let e = status.lastError, !status.isActive {
                Text(e).font(.caption2).foregroundStyle(.secondary).lineLimit(3).multilineTextAlignment(.center)
            }
        }
    }

    private var title: String {
        switch status.phase {
        case "active": status.walking == true ? "Moving" : (status.target == nil ? "Connected" : "Spoofing")
        case "connecting": "Connecting…"
        case "mounting_ddi": "Mounting developer image…"
        case "retrying": "Reconnecting…"
        default: "Real GPS"
        }
    }

    private var color: Color {
        switch status.phase {
        case "active": .green
        case "connecting", "mounting_ddi": .yellow
        case "retrying": .orange
        default: .gray
        }
    }
}
