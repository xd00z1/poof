import CoreLocation
import MapKit

/// How the spoofed position travels to a destination. Every mode follows
/// real paths from Apple Maps directions: roads for driving, walkable paths
/// for the rest.
enum TravelMode: String, CaseIterable, Identifiable {
    case walk, run, bike, drive

    var id: String { rawValue }

    var label: String {
        switch self {
        case .walk: "Walk"
        case .run: "Run"
        case .bike: "Bike"
        case .drive: "Drive"
        }
    }

    var icon: String {
        switch self {
        case .walk: "figure.walk"
        case .run: "figure.run"
        case .bike: "bicycle"
        case .drive: "car.fill"
        }
    }

    /// Typical average speed, metres per second.
    var defaultMps: Double {
        switch self {
        case .walk: 1.4
        case .run: 2.9
        case .bike: 5.0
        case .drive: 13.4
        }
    }

    /// Apple has no cycling directions everywhere; bikes use walking paths.
    var transport: MKDirectionsTransportType {
        self == .drive ? .automobile : .walking
    }
}

/// Average driving speeds offered in the UI. `nil` means "match Apple's
/// estimated travel time", which accounts for road types and traffic.
enum DriveSpeed: Int, CaseIterable, Identifiable {
    case auto = 0
    case mph25 = 25, mph35 = 35, mph45 = 45, mph55 = 55, mph65 = 65

    var id: Int { rawValue }
    var label: String { self == .auto ? "Like traffic (Apple ETA)" : "\(rawValue) mph" }
    var mps: Double? { self == .auto ? nil : Double(rawValue) * 0.44704 }
}

struct PlannedRoute {
    let points: [CLLocationCoordinate2D]
    /// Metres along the roads.
    let distance: Double
    /// Apple's estimate for this mode, seconds.
    let expectedTravelTime: TimeInterval
}

enum RoutingError: LocalizedError {
    case noRoute

    var errorDescription: String? {
        "Apple Maps found no route there. Pick a spot on or near a road."
    }
}

enum Router {
    static func route(
        from: CLLocationCoordinate2D,
        to: CLLocationCoordinate2D,
        mode: TravelMode
    ) async throws -> PlannedRoute {
        let request = MKDirections.Request()
        request.source = MKMapItem(placemark: MKPlacemark(coordinate: from))
        request.destination = MKMapItem(placemark: MKPlacemark(coordinate: to))
        request.transportType = mode.transport
        request.requestsAlternateRoutes = false

        let response = try await MKDirections(request: request).calculate()
        guard let route = response.routes.first else { throw RoutingError.noRoute }

        let polyline = route.polyline
        var coords = [CLLocationCoordinate2D](
            repeating: kCLLocationCoordinate2DInvalid, count: polyline.pointCount
        )
        polyline.getCoordinates(&coords, range: NSRange(location: 0, length: polyline.pointCount))
        // Start exactly at the current spoof and end at the chosen spot, so
        // there is no jump onto or off the road network.
        coords.insert(from, at: 0)
        coords.append(to)
        return PlannedRoute(points: coords, distance: route.distance, expectedTravelTime: route.expectedTravelTime)
    }
}
