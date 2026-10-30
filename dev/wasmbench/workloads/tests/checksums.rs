//! The workloads' checksums, as the C they replace printed them (bench/fb.c
//! and bench/float.c, at the commit proposal 0003's numbers came from), for
//! the sizes tools/stage.sh's plans run: the quick plan's, the full plan's
//! and the binaries' defaults. On the Chumby, the harness checks that every
//! runtime prints the same as native; this checks that native prints what
//! the C did.

use workloads::{fb, float};

/// fb's four checksums after `frames` frames, as the binary computes them.
fn fb_sums(frames: i32) -> [u32; 4] {
    let mut back = vec![0u16; fb::W * fb::H];
    let mut front = vec![0u16; fb::W * fb::H];
    let mut sprite = vec![0u16; fb::S * fb::S];
    fb::fill(&mut back, frames);
    let fill = fb::sum(&back);
    fb::copy(&mut back, &mut front, frames);
    let copy = fb::sum(&front);
    fb::sprite(&mut sprite);
    fb::blit(&mut front, &sprite, frames);
    let blit = fb::sum(&front);
    fb::rect(&mut front, frames);
    [fill, copy, blit, fb::sum(&front)]
}

/// float's two checksums, printed as the binary prints them.
fn float_sums(steps: i32, reps: i32) -> (String, String) {
    let mut bodies = float::bodies();
    for _ in 0..steps {
        float::advance(&mut bodies, 0.01);
    }
    let (mut a, b) = float::matrices();
    let mut c = [[0.0; float::N]; float::N];
    float::matmul(&mut a, &b, &mut c, reps);
    (format!("{:.9}", float::energy(&bodies)), format!("{:.4}", float::matsum(&c)))
}

#[test]
fn fb_quick() {
    assert_eq!(fb_sums(5), [2642878464, 4121630366, 1912279894, 1649360037]);
}

#[test]
fn fb_full() {
    assert_eq!(fb_sums(100), [1276977152, 1055745202, 1403129919, 2837834005]);
}

#[test]
fn float_quick() {
    assert_eq!(float_sums(200, 2), ("-0.169026909".into(), "2720.2500".into()));
}

#[test]
fn float_full() {
    assert_eq!(float_sums(5000, 20), ("-0.169020000".into(), "2666.2500".into()));
}

#[test]
fn float_defaults() {
    assert_eq!(float_sums(20000, 20), ("-0.169089263".into(), "2666.2500".into()));
}
