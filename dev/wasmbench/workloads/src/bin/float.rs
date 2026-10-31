//! float: the soft-float kernel (workloads::float), each part timed.
//!
//!   float [STEPS [REPS]]    20000 n-body steps and 20 multiplies by default
//!
//! Prints one line per part: its name, the work done, the milliseconds and a
//! checksum (the same under every runtime, or something is wrong).

use std::time::Instant;

use workloads::float;
use workloads::{arg, ms};

fn main() {
    let steps = arg(1, 20000);
    let reps = arg(2, 20);

    let mut bodies = float::bodies();
    let t0 = Instant::now();
    for _ in 0..steps {
        float::advance(&mut bodies, 0.01);
    }
    let t = ms(t0);
    println!("nbody steps={steps} ms={t:.1} energy={:.9}", float::energy(&bodies));

    let (mut a, b) = float::matrices();
    let mut c = [[0.0; float::N]; float::N];
    let t0 = Instant::now();
    float::matmul(&mut a, &b, &mut c, reps);
    let t = ms(t0);
    println!("matmul reps={reps} ms={t:.1} sum={:.4}", float::matsum(&c));
}
