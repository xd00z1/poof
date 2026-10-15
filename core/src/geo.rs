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

    /// Initial great-circle bearing to `other`, degrees clockwise from north.
    pub fn bearing_to(&self, other: &Coord) -> f64 {
        let (p1, p2) = (self.lat.to_radians(), other.lat.to_radians());
        let dl = (other.lon - self.lon).to_radians();
        let y = dl.sin() * p2.cos();
        let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
        y.atan2(x).to_degrees().rem_euclid(360.0)
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
    /// Drive like a person: speed drifts ±15% around `speed_mps` and slows
    /// for sharp turns, instead of a constant-speed robot.
    #[serde(default)]
    pub vary: bool,
}

/// Speed through a sharp turn (≈ 22 km/h).
const TURN_SPEED_MPS: f64 = 6.0;
/// Start slowing this far before a sharp turn, and keep the speed down for the
/// same distance after it.
const TURN_ZONE_M: f64 = 25.0;
/// Turns sharper than this count as sharp (degrees of heading change).
const SHARP_TURN_DEG: f64 = 45.0;

/// Position along a route, advanced by elapsed time.
#[derive(Debug, Clone)]
pub struct Mover {
    route: Route,
    segment: usize,
    /// Metres already travelled along the current segment.
    along: f64,
    /// Current multiplier on the base speed when `vary` is set.
    factor: f64,
    rng: u64,
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
        // A looping route with no length would make `advance` spin forever.
        let length: f64 = route
            .points
            .windows(2)
            .map(|w| w[0].distance_to(&w[1]))
            .sum();
        let valid = valid && !(route.repeat && length < 1.0);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        valid.then_some(Self {
            route,
            segment: 0,
            along: 0.0,
            factor: 1.0,
            rng: seed | 1,
        })
    }

    /// Metres left to the end of the route (one lap when looping).
    pub fn remaining_m(&self) -> f64 {
        let pts = &self.route.points;
        if self.segment + 1 >= pts.len() {
            return 0.0;
        }
        let current = pts[self.segment].distance_to(&pts[self.segment + 1]) - self.along;
        let rest: f64 = pts[self.segment + 1..]
            .windows(2)
            .map(|w| w[0].distance_to(&w[1]))
            .sum();
        (current + rest).max(0.0)
    }

    /// Seconds to the end at the base speed.
    pub fn eta_secs(&self) -> f64 {
        self.remaining_m() / self.route.speed_mps
    }

    /// Uniform in [-1, 1) (xorshift64).
    fn noise(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// Heading change at vertex `i`, in degrees (0 = straight on).
    fn turn_deg(&self, i: usize) -> f64 {
        let pts = &self.route.points;
        if i == 0 || i + 1 >= pts.len() {
            return 0.0;
        }
        let (a, b) = (
            pts[i - 1].bearing_to(&pts[i]),
            pts[i].bearing_to(&pts[i + 1]),
        );
        let d = (b - a).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    /// Speed right now: the base speed, drifted and slowed for turns if `vary`.
    fn speed_here(&self) -> f64 {
        let base = self.route.speed_mps;
        if !self.route.vary {
            return base;
        }
        let pts = &self.route.points;
        let v = base * self.factor;
        if self.segment + 1 >= pts.len() {
            return v;
        }
        let len = pts[self.segment].distance_to(&pts[self.segment + 1]);
        let near_end =
            len - self.along < TURN_ZONE_M && self.turn_deg(self.segment + 1) > SHARP_TURN_DEG;
        let near_start = self.along < TURN_ZONE_M && self.turn_deg(self.segment) > SHARP_TURN_DEG;
        if near_end || near_start {
            v.min(TURN_SPEED_MPS)
        } else {
            v
        }
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
        if self.route.vary {
            // Slow random walk of the speed, kept within ±15%.
            self.factor = (self.factor + 0.04 * self.noise() * secs.min(5.0)).clamp(0.85, 1.15);
        }
        // Sub-steps so turn slow-downs apply where they happen.
        let mut time = secs.max(0.0);
        while time > 0.0 && !self.finished() {
            let dt = time.min(0.25);
            time -= dt;
            let mut remaining = self.speed_here() * dt;
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
            vary: false,
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
            vary: false,
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
                repeat: false,
                vary: false
            })
            .is_none()
        );
        assert!(
            Mover::new(Route {
                points: vec![c(0.0, 0.0), c(1.0, 1.0)],
                speed_mps: 0.0,
                repeat: false,
                vary: false
            })
            .is_none()
        );
    }

    #[test]
    fn rejects_zero_length_loop() {
        let p = c(1.0, 1.0);
        let r = |repeat| Route {
            points: vec![p, p],
            speed_mps: 1.0,
            repeat,
            vary: false,
        };
        assert!(Mover::new(r(true)).is_none());
        // Non-repeating is fine: it finishes on the first step.
        let mut m = Mover::new(r(false)).unwrap();
        assert_eq!(m.advance(1.0), p);
        assert!(m.finished());
    }

    #[test]
    fn bearing_cardinal() {
        let o = c(0.0, 0.0);
        assert!((o.bearing_to(&c(1.0, 0.0)) - 0.0).abs() < 1e-6);
        assert!((o.bearing_to(&c(0.0, 1.0)) - 90.0).abs() < 1e-6);
    }

    #[test]
    fn remaining_and_eta() {
        let (a, b) = (c(0.0, 0.0), c(0.0, 0.01)); // ~1.1 km
        let len = a.distance_to(&b);
        let mut m = Mover::new(Route {
            points: vec![a, b],
            speed_mps: 10.0,
            repeat: false,
            vary: false,
        })
        .unwrap();
        assert!((m.remaining_m() - len).abs() < 0.5);
        m.advance(10.0);
        assert!((m.remaining_m() - (len - 100.0)).abs() < 0.5);
        assert!((m.eta_secs() - (len - 100.0) / 10.0).abs() < 0.1);
    }

    #[test]
    fn varied_speed_stays_in_band_and_slows_for_turns() {
        // East 2 km, then a 90° turn north for 2 km.
        let pts = vec![c(0.0, 0.0), c(0.0, 0.018), c(0.018, 0.018)];
        let total: f64 = pts.windows(2).map(|w| w[0].distance_to(&w[1])).sum();
        let mut m = Mover::new(Route {
            points: pts,
            speed_mps: 20.0,
            repeat: false,
            vary: true,
        })
        .unwrap();
        let mut secs = 0.0;
        while !m.finished() && secs < 1000.0 {
            m.advance(1.0);
            secs += 1.0;
        }
        assert!(m.finished());
        // ±15% drift plus a short slow zone at the corner.
        let nominal = total / 20.0;
        assert!(
            secs > nominal * 0.85 && secs < nominal * 1.25,
            "{secs} vs {nominal}"
        );

        // Speed through the corner is capped.
        let mut m = Mover::new(Route {
            points: vec![c(0.0, 0.0), c(0.0, 0.018), c(0.018, 0.018)],
            speed_mps: 20.0,
            repeat: false,
            vary: true,
        })
        .unwrap();
        let corner = c(0.0, 0.018);
        while m.position().distance_to(&corner) > 20.0 {
            m.advance(0.25);
        }
        let before = m.position();
        m.advance(1.0);
        assert!(before.distance_to(&m.position()) <= TURN_SPEED_MPS + 0.5);
    }
}
