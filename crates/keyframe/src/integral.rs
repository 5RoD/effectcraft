//! Prepared integral of a scalar keyframe curve. Queries are independent of playback order.
//! Bézier integration uses its parametric value × time derivative (a degree-five polynomial),
//! so the existing five-point Gauss rule is exact up to floating-point rounding.

use super::{Ease, GAUSS, Interp, Keyframe, Value, bez, bez_d, effective_ease, solve_u};
use effectcraft_time::{TICKS_PER_SECOND, Tick};

const MAX_KEYS: usize = 1_000_000;

#[derive(Clone, Debug)]
enum Shape {
    Hold(f64),
    Linear(f64, f64),
    Bezier { x: [f64; 4], y: [f64; 4] },
}

#[derive(Clone, Debug)]
struct Segment {
    start: Tick,
    end: Tick,
    prefix: f64,
    shape: Shape,
}

/// A validated scalar curve, with cumulative areas at the key boundaries. Area units are
/// value × Ticks; callers can divide by a rate without passing exact boundaries through seconds.
#[derive(Clone, Debug)]
pub struct ScalarIntegral {
    first: Tick,
    last: Tick,
    first_value: f64,
    last_value: f64,
    area: f64,
    constant: Option<f64>,
    segments: Vec<Segment>,
}

fn finite(value: f64) -> Result<f64, String> {
    if value.is_finite() { Ok(value) } else { Err("scalar integral exceeds finite arithmetic".into()) }
}

fn delta(a: Tick, b: Tick) -> f64 {
    (b.0 as i128 - a.0 as i128) as f64
}

impl Segment {
    fn area_to(&self, t: Tick) -> f64 {
        let duration = delta(self.start, self.end);
        let x = (delta(self.start, t) / duration).clamp(0.0, 1.0);
        match self.shape {
            Shape::Hold(value) => value * duration * x,
            Shape::Linear(a, b) => duration * x * (a + (b - a) * x * 0.5),
            Shape::Bezier { x: cx, y } => {
                let u = solve_u(cx[0], cx[1], cx[2], cx[3], x);
                let half = u * 0.5;
                duration
                    * half
                    * GAUSS
                        .iter()
                        .map(|(node, weight)| {
                            let at = half * (node + 1.0);
                            weight * bez(y[0], y[1], y[2], y[3], at) * bez_d(cx[0], cx[1], cx[2], cx[3], at)
                        })
                        .sum::<f64>()
            }
        }
    }
}

impl ScalarIntegral {
    pub fn new(keys: &[Keyframe]) -> Result<Self, String> {
        if keys.len() > MAX_KEYS {
            return Err("scalar integral exceeds the one-million-key budget".into());
        }
        let scalar = |key: &Keyframe| match key.value {
            Value::Scalar(value) => finite(value),
            _ => Err("scalar integral requires numeric scalar keys".into()),
        };
        let first = keys.first().ok_or("scalar integral requires at least one key")?;
        let last = keys.last().ok_or("scalar integral requires at least one key")?;
        for key in keys {
            scalar(key)?;
            if !(Tick::MIN..=Tick::MAX).contains(&key.time) {
                return Err("scalar integral key time exceeds the supported Tick range".into());
            }
            if key.in_ease.iter().chain(&key.out_ease).any(|e| !e.speed.is_finite() || !e.influence.is_finite()) {
                return Err("scalar integral requires finite easing handles".into());
            }
        }
        let mut segments = Vec::new();
        segments.try_reserve_exact(keys.len().saturating_sub(1)).map_err(|e| format!("cannot allocate scalar integral: {e}"))?;
        let mut area = 0.0;
        for (i, pair) in keys.windows(2).enumerate() {
            let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else { return Err("scalar integral segment is incomplete".into()) };
            if b.time <= a.time {
                return Err("scalar integral keys must have strictly increasing times".into());
            }
            let (av, bv) = (scalar(a)?, scalar(b)?);
            let linear_out = a.out_interp == Interp::Linear;
            let linear_in = b.in_interp != Interp::Bezier;
            let shape = if a.out_interp == Interp::Hold {
                Shape::Hold(av)
            } else if linear_out && linear_in {
                Shape::Linear(av, bv)
            } else {
                let seconds = delta(a.time, b.time) / TICKS_PER_SECOND as f64;
                let linear = Ease { speed: (bv - av) / seconds, influence: 1.0 / 3.0 };
                let out = if linear_out { linear } else { effective_ease(keys, i, 0, false, true) };
                let incoming = if linear_in { linear } else { effective_ease(keys, i + 1, 0, false, false) };
                let (o, n) = (out.influence.clamp(0.0, 1.0), incoming.influence.clamp(0.0, 1.0));
                let y = [av, finite(av + out.speed * o * seconds)?, finite(bv - incoming.speed * n * seconds)?, bv];
                Shape::Bezier { x: [0.0, o, 1.0 - n, 1.0], y }
            };
            let segment = Segment { start: a.time, end: b.time, prefix: area, shape };
            area = finite(area + segment.area_to(b.time))?;
            segments.push(segment);
        }
        let first_value = scalar(first)?;
        let constant = (keys.iter().all(|k| matches!(k.value, Value::Scalar(v) if v == first_value))
            && segments.iter().all(|s| match s.shape {
                Shape::Hold(v) => v == first_value,
                Shape::Linear(a, b) => a == first_value && b == first_value,
                Shape::Bezier { y, .. } => y.iter().all(|v| *v == first_value),
            }))
        .then_some(first_value);
        Ok(Self { first: first.time, last: last.time, first_value, last_value: scalar(last)?, area, constant, segments })
    }

    fn primitive(&self, t: Tick) -> Result<f64, String> {
        if !(Tick::MIN..=Tick::MAX).contains(&t) {
            return Err("scalar integral time exceeds the supported Tick range".into());
        }
        if t <= self.first {
            return finite(self.first_value * delta(self.first, t));
        }
        if t >= self.last {
            return finite(self.area + self.last_value * delta(self.last, t));
        }
        let i = self.segments.partition_point(|s| s.end <= t);
        let segment = self.segments.get(i).ok_or("scalar integral segment is missing")?;
        finite(segment.prefix + segment.area_to(t))
    }

    pub fn between(&self, from: Tick, to: Tick) -> Result<f64, String> {
        finite(self.primitive(to)? - self.primitive(from)?)
    }

    pub fn constant_value(&self) -> Option<f64> {
        self.constant
    }

    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.segments.capacity().saturating_mul(std::mem::size_of::<Segment>()))
    }
}

/// Bounded adaptive Simpson integration for expression-driven rates. The argument is a
/// fraction of the interval (0..1), keeping numerical work independent of project duration.
/// Discontinuous or difficult functions that cannot meet the tolerance return an error.
pub fn integrate_function(mut value: impl FnMut(f64) -> Result<f64, String>) -> Result<f64, String> {
    struct Part {
        a: f64,
        b: f64,
        fa: f64,
        fm: f64,
        fb: f64,
        area: f64,
        tolerance: f64,
        depth: u8,
    }
    let (fa, fm, fb) = (finite(value(0.0)?)?, finite(value(0.5)?)?, finite(value(1.0)?)?);
    let area = finite((fa + 4.0 * fm + fb) / 6.0)?;
    let mut stack = vec![Part { a: 0.0, b: 1.0, fa, fm, fb, area, tolerance: 1e-9 * area.abs().max(1.0), depth: 0 }];
    let mut result = 0.0;
    let mut evaluations = 3usize;
    while let Some(p) = stack.pop() {
        if evaluations > 1023 {
            return Err("rate expression exceeds the integration work budget; animate Source Frame for an explicit mapping".into());
        }
        let m = (p.a + p.b) * 0.5;
        let (lm, rm) = ((p.a + m) * 0.5, (m + p.b) * 0.5);
        let (fl, fr) = (finite(value(lm)?)?, finite(value(rm)?)?);
        evaluations += 2;
        let left = finite((m - p.a) * (p.fa + 4.0 * fl + p.fm) / 6.0)?;
        let right = finite((p.b - m) * (p.fm + 4.0 * fr + p.fb) / 6.0)?;
        let error = finite(left + right - p.area)?;
        if error.abs() <= 15.0 * p.tolerance {
            result = finite(result + left + right + error / 15.0)?;
        } else {
            if p.depth >= 16 {
                return Err("rate expression integration did not converge; animate Source Frame for an explicit mapping".into());
            }
            let depth = p.depth + 1;
            let tolerance = p.tolerance * 0.5;
            stack.push(Part { a: m, b: p.b, fa: p.fm, fm: fr, fb: p.fb, area: right, tolerance, depth });
            stack.push(Part { a: p.a, b: m, fa: p.fa, fm: fl, fb: p.fm, area: left, tolerance, depth });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(seconds: f64) -> Tick {
        Tick::from_seconds_f64(seconds)
    }
    fn key(seconds: f64, value: f64) -> Keyframe {
        Keyframe::new(t(seconds), Value::Scalar(value))
    }
    fn seconds(curve: &ScalarIntegral, from: f64, to: f64) -> f64 {
        curve.between(t(from), t(to)).unwrap() / TICKS_PER_SECOND as f64
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn linear_hold_freeze_reverse_and_extrapolation_are_integrated() {
        let curve = ScalarIntegral::new(&[key(0.0, 50.0), key(2.0, 100.0)]).unwrap();
        near(seconds(&curve, 0.0, 1.0), 62.5);
        near(seconds(&curve, 0.0, 2.0), 150.0);
        near(seconds(&curve, -1.0, 3.0), 300.0);
        near(seconds(&curve, 2.0, 0.0), -150.0);
        let hold = ScalarIntegral::new(&[key(0.0, 100.0).hold(), key(1.0, 0.0).hold(), key(2.0, -100.0)]).unwrap();
        near(seconds(&hold, 0.0, 1.0), 100.0);
        near(seconds(&hold, 0.0, 2.0), 100.0);
        near(seconds(&hold, 0.0, 3.0), 0.0);
        near(seconds(&hold, 0.0, -1.0), -100.0);
    }

    #[test]
    fn bezier_area_matches_an_independent_dense_time_oracle() {
        let smooth = ScalarIntegral::new(&[key(0.0, 50.0).eased(), key(1.0, 100.0).eased()]).unwrap();
        near(seconds(&smooth, 0.0, 1.0), 75.0);
        near(seconds(&smooth, 0.0, 0.5), 29.6875);
        for (out, incoming) in [(0.33, 0.33), (0.9, 0.95), (1.0, 1.0), (0.0, 0.0)] {
            let mut a = key(-0.5, 40.0).eased();
            let mut b = key(1.5, 140.0).eased();
            a.out_ease = vec![Ease { speed: 210.0, influence: out }];
            b.in_ease = vec![Ease { speed: -40.0, influence: incoming }];
            let keys = [a, b];
            let curve = ScalarIntegral::new(&keys).unwrap();
            for end in [0.3, 1.0, 1.5] {
                let n = 80_000;
                let width = (end + 0.5) / n as f64;
                let reference = (0..n).map(|i| super::super::evaluate(&keys, t(-0.5 + (i as f64 + 0.5) * width), false).unwrap().as_f64() * width).sum::<f64>();
                let actual = seconds(&curve, -0.5, end);
                assert!((actual - reference).abs() < 0.003, "handles {out}/{incoming}, end {end}: {actual} != {reference}");
            }
        }
    }

    #[test]
    fn auto_ease_partial_intervals_and_constant_identity() {
        let mut middle = key(1.0, 150.0).eased();
        middle.auto_bezier = true;
        let keys = [key(0.0, 50.0).eased(), middle, key(3.0, -50.0).eased()];
        let curve = ScalarIntegral::new(&keys).unwrap();
        let a = seconds(&curve, 0.0, 3.0);
        near(a, seconds(&curve, 0.0, 0.7) + seconds(&curve, 0.7, 2.1) + seconds(&curve, 2.1, 3.0));
        let constant = ScalarIntegral::new(&[key(-1.0, 100.0).eased(), key(1.0, 100.0).eased()]).unwrap();
        assert_eq!(constant.constant_value(), Some(100.0));
    }

    #[test]
    fn invalid_curves_fail_without_preventing_later_valid_queries() {
        for keys in [
            vec![],
            vec![key(0.0, f64::NAN)],
            vec![key(1.0, 1.0), key(0.0, 2.0)],
            vec![key(0.0, 1.0), key(0.0, 2.0)],
            vec![Keyframe::new(t(0.0), Value::Bool(true))],
        ] {
            assert!(ScalarIntegral::new(&keys).is_err());
        }
        let mut invalid = key(0.0, 10.0).eased();
        invalid.out_ease[0].speed = f64::INFINITY;
        assert!(ScalarIntegral::new(&[invalid, key(1.0, 20.0)]).is_err());
        near(seconds(&ScalarIntegral::new(&[key(0.0, 100.0)]).unwrap(), 0.0, 1.0), 100.0);
    }

    #[test]
    fn expression_integral_is_bounded_checked_and_accurate_for_smooth_rates() {
        near(integrate_function(|u| Ok(50.0 + 100.0 * u)).unwrap(), 100.0);
        near(integrate_function(|u| Ok(u * u)).unwrap(), 1.0 / 3.0);
        assert!(integrate_function(|_| Ok(f64::NAN)).is_err());
        assert!(integrate_function(|_| Err("expression failed".into())).unwrap_err().contains("expression failed"));
        let mut calls = 0;
        assert!(
            integrate_function(|_| {
                calls += 1;
                Ok(if calls % 2 == 0 { 1.0 } else { 0.0 })
            })
            .is_err()
        );
        assert!(calls <= 1027, "bounded evaluations: {calls}");
    }
}
