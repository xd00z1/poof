//! Coordinates and route movement.
//!
//! A teleport across a continent is the easiest spoof to flag. `Mover` walks a
//! polyline at a fixed speed so the reported position changes the way a real
//! one would.

use serde::{Deserialize, Serialize};

const EARTH_RADIUS_M: f64 = 6_371_008.8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Coord {
    pub lat: f64,
    pub lon: f64,
}

impl Coord {
    pub fn new(lat: f64, lon: f64) -> Option<Self> {
        let ok = lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon);
        ok.then_some(Self { lat, lon })
    }

    /// Great-circle distance in metres (haversine).
    pub fn distance_to(&self, other: &Coord) -> f64 {
        let (p1, p2) = (self.lat.to_radians(), other.lat.to_radians());
        let dp = p2 - p1;
        let dl = (other.lon - self.lon).to_radians();
        let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_M * a.sqrt().min(1.0).asin()
    }

    /// Linear interpolation. Adequate for the short segments of a walked route;
    /// longitude is unwrapped so a segment crossing the antimeridian takes the
    /// short way round.
    pub fn lerp(&self, other: &Coord, t: f64) -> Coord {
        let mut dlon = other.lon - self.lon;
        if dlon > 180.0 {
            dlon -= 360.0;
        } else if dlon < -180.0 {
            dlon += 360.0;
        }
        let mut lon = self.lon + dlon * t;
        if lon > 180.0 {
            lon -= 360.0;
        } else if lon < -180.0 {
            lon += 360.0;
        }
        Coord {
            lat: self.lat + (other.lat - self.lat) * t,
            lon,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub points: Vec<Coord>,
    /// Metres per second. Walking ≈ 1.4, cycling ≈ 5, driving ≈ 13.
    pub speed_mps: f64,
    /// Start over from the first point after reaching the last one.
    #[serde(default, rename = "loop")]
    pub repeat: bool,
}

/// Position along a route, advanced by elapsed time.
#[derive(Debug, Clone)]
pub struct Mover {
    route: Route,
    segment: usize,
    /// Metres already travelled along the current segment.
    along: f64,
}

impl Mover {
    pub fn new(route: Route) -> Option<Self> {
        let valid = route.points.len() >= 2
            && route.speed_mps.is_finite()
            && route.speed_mps > 0.0
            && route
                .points
                .iter()
                .all(|c| Coord::new(c.lat, c.lon).is_some());
        valid.then_some(Self {
            route,
            segment: 0,
            along: 0.0,
        })
    }

    pub fn position(&self) -> Coord {
        let pts = &self.route.points;
        if self.segment + 1 >= pts.len() {
            return *pts.last().unwrap();
        }
        let (a, b) = (pts[self.segment], pts[self.segment + 1]);
        let len = a.distance_to(&b);
        if len <= f64::EPSILON {
            return a;
        }
        a.lerp(&b, (self.along / len).clamp(0.0, 1.0))
    }

    pub fn finished(&self) -> bool {
        !self.route.repeat && self.segment + 1 >= self.route.points.len()
    }

    /// Advances by `secs` of travel and returns the new position.
    pub fn advance(&mut self, secs: f64) -> Coord {
        let mut remaining = self.route.speed_mps * secs.max(0.0);
        let pts = &self.route.points;
        while remaining > 0.0 && !self.finished() {
            if self.segment + 1 >= pts.len() {
                // Only reachable when repeating.
                self.segment = 0;
                self.along = 0.0;
                continue;
            }
            let len = pts[self.segment].distance_to(&pts[self.segment + 1]);
            let left = len - self.along;
            if remaining < left {
                self.along += remaining;
                remaining = 0.0;
            } else {
                remaining -= left;
                self.segment += 1;
                self.along = 0.0;
            }
        }
        self.position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lat: f64, lon: f64) -> Coord {
        Coord::new(lat, lon).unwrap()
    }

    #[test]
    fn rejects_bad_coords() {
        assert!(Coord::new(91.0, 0.0).is_none());
        assert!(Coord::new(0.0, -180.5).is_none());
        assert!(Coord::new(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn haversine_known_distance() {
        // Paris (Notre-Dame) to London (Charing Cross): ~343.5 km.
        let d = c(48.8530, 2.3499).distance_to(&c(51.5073, -0.1276));
        assert!((d - 343_500.0).abs() < 2_000.0, "{d}");
    }

    #[test]
    fn walks_at_speed_and_stops() {
        let a = c(0.0, 0.0);
        let b = c(0.0, 0.001); // ~111 m east
        let len = a.distance_to(&b);
        let mut m = Mover::new(Route {
            points: vec![a, b],
            speed_mps: 10.0,
            repeat: false,
        })
        .unwrap();
        let p = m.advance(5.0);
        assert!((a.distance_to(&p) - 50.0).abs() < 0.5);
        assert!(!m.finished());
        let p = m.advance(len); // far past the end
        assert_eq!(p, b);
        assert!(m.finished());
    }

    #[test]
    fn repeating_route_wraps() {
        let a = c(0.0, 0.0);
        let b = c(0.0, 0.001);
        let len = a.distance_to(&b);
        let mut m = Mover::new(Route {
            points: vec![a, b],
            speed_mps: 1.0,
            repeat: true,
        })
        .unwrap();
        let p = m.advance(len + 10.0);
        assert!(!m.finished());
        assert!(
            (a.distance_to(&p) - 10.0).abs() < 0.5,
            "{}",
            a.distance_to(&p)
        );
    }

    #[test]
    fn antimeridian_takes_short_way() {
        let p = c(0.0, 179.0).lerp(&c(0.0, -179.0), 0.5);
        assert!((p.lon.abs() - 180.0).abs() < 1e-9, "{}", p.lon);
    }

    #[test]
    fn rejects_degenerate_routes() {
        assert!(
            Mover::new(Route {
                points: vec![c(0.0, 0.0)],
                speed_mps: 1.0,
                repeat: false
            })
            .is_none()
        );
        assert!(
            Mover::new(Route {
                points: vec![c(0.0, 0.0), c(1.0, 1.0)],
                speed_mps: 0.0,
                repeat: false
            })
            .is_none()
        );
    }
}
