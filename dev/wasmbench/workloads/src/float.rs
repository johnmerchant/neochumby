//! The soft-float kernel: the Chumby has no FPU, so every f32 and f64
//! operation is a call into the soft-float helpers natively, and the same
//! calls (or the interpreter's own) under wasm. Two parts, which the `float`
//! binary times one by one:
//!
//!   nbody   f64: the five-body step of the Benchmarks Game, with sqrt
//!   matmul  f32: a 32x32 matrix multiply, repeated
//!
//! Each operation is written in the order the C it replaces wrote it, so
//! the results, rounded the same at every step, are the same to the bit.

use std::f64::consts::PI;
const SOLAR_MASS: f64 = 4.0 * PI * PI;
const DAYS: f64 = 365.24;

/// A body: its position, its velocity and its mass.
#[derive(Clone, Copy)]
pub struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    m: f64,
}

/// The Sun, Jupiter, Saturn, Uranus and Neptune, with the system's
/// momentum taken off the Sun so that it stays put. (The constants are the
/// Benchmarks Game's, digit for digit, though an f64 holds fewer.)
#[allow(clippy::excessive_precision)]
pub fn bodies() -> [Body; 5] {
    #[rustfmt::skip]
    let mut b = [
        Body { x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, m: SOLAR_MASS },
        Body {
            x: 4.84143144246472090e+00, y: -1.16032004402742839e+00, z: -1.03622044471123109e-01,
            vx: 1.66007664274403694e-03 * DAYS, vy: 7.69901118419740425e-03 * DAYS,
            vz: -6.90460016972063023e-05 * DAYS, m: 9.54791938424326609e-04 * SOLAR_MASS,
        },
        Body {
            x: 8.34336671824457987e+00, y: 4.12479856412430479e+00, z: -4.03523417114321381e-01,
            vx: -2.76742510726862411e-03 * DAYS, vy: 4.99852801234917238e-03 * DAYS,
            vz: 2.30417297573763929e-05 * DAYS, m: 2.85885980666130812e-04 * SOLAR_MASS,
        },
        Body {
            x: 1.28943695621391310e+01, y: -1.51111514016986312e+01, z: -2.23307578892655734e-01,
            vx: 2.96460137564761618e-03 * DAYS, vy: 2.37847173959480950e-03 * DAYS,
            vz: -2.96589568540237556e-05 * DAYS, m: 4.36624404335156298e-05 * SOLAR_MASS,
        },
        Body {
            x: 1.53796971148509165e+01, y: -2.59193146099879641e+01, z: 1.79258772950371181e-01,
            vx: 2.68067772490389322e-03 * DAYS, vy: 1.62824170038242295e-03 * DAYS,
            vz: -9.51592254519715870e-05 * DAYS, m: 5.15138902046611451e-05 * SOLAR_MASS,
        },
    ];
    let (mut px, mut py, mut pz) = (0.0, 0.0, 0.0);
    for a in &b {
        px += a.vx * a.m;
        py += a.vy * a.m;
        pz += a.vz * a.m;
    }
    b[0].vx = -px / SOLAR_MASS;
    b[0].vy = -py / SOLAR_MASS;
    b[0].vz = -pz / SOLAR_MASS;
    b
}

/// Moves the bodies on by `dt`: each pair's pull, then each one's step.
pub fn advance(bodies: &mut [Body; 5], dt: f64) {
    for i in 0..5 {
        let (head, tail) = bodies.split_at_mut(i + 1);
        let a = &mut head[i];
        for b in tail {
            let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            a.vx -= dx * b.m * mag;
            a.vy -= dy * b.m * mag;
            a.vz -= dz * b.m * mag;
            b.vx += dx * a.m * mag;
            b.vy += dy * a.m * mag;
            b.vz += dz * a.m * mag;
        }
    }
    for b in bodies {
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

/// The system's energy, kinetic less potential: the check that n-body did
/// the same work.
pub fn energy(bodies: &[Body; 5]) -> f64 {
    let mut e = 0.0;
    for (i, a) in bodies.iter().enumerate() {
        e += 0.5 * a.m * (a.vx * a.vx + a.vy * a.vy + a.vz * a.vz);
        for b in &bodies[i + 1..] {
            let (dx, dy, dz) = (a.x - b.x, a.y - b.y, a.z - b.z);
            e -= a.m * b.m / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}

/// The matrices' side.
pub const N: usize = 32;

/// An N x N matrix of f32, row by row.
pub type Matrix = [[f32; N]; N];

/// The two matrices multiplied: `a[i][j]` is (i + j) / N, `b[i][j]` is
/// (i - j) / N.
pub fn matrices() -> (Matrix, Matrix) {
    let mut a = [[0.0; N]; N];
    let mut b = [[0.0; N]; N];
    for i in 0..N {
        for j in 0..N {
            a[i][j] = (i as i32 + j as i32) as f32 / N as f32;
            b[i][j] = (i as i32 - j as i32) as f32 / N as f32;
        }
    }
    (a, b)
}

/// Multiplies `a` by `b` into `c`, `reps` times, nudging one element of `a`
/// on the diagonal after each.
pub fn matmul(a: &mut Matrix, b: &Matrix, c: &mut Matrix, reps: i32) {
    for r in 0..reps {
        for i in 0..N {
            for j in 0..N {
                let mut s = 0.0f32;
                for k in 0..N {
                    s += a[i][k] * b[k][j];
                }
                c[i][j] = s;
            }
        }
        let d = r as usize % N;
        a[d][d] += 0.5;
    }
}

/// The sum of `c`'s elements, in f32, row by row: the check that matmul
/// did the same work.
pub fn matsum(c: &Matrix) -> f32 {
    let mut sum = 0.0f32;
    for row in c {
        for &x in row {
            sum += x;
        }
    }
    sum
}
