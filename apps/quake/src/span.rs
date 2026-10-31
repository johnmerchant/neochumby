//! D_DrawSpans8 (the textured span drawer, d_scan.c) without soft float in
//! the loop. Profiling put a third of all time in it, two thirds of that in
//! the soft-float calls made every 8 pixels for perspective correction:
//! three adds, a divide, two multiplies, two conversions.
//!
//! Here floats are used only at the two ends of each span. s/z, t/z and 1/z
//! are then fixed point, each scaled per span from its float exponent so it
//! sits just under 2^30 (no overflow, full precision near or far), stepped
//! with integer adds; the divide is a reciprocal: CLZ, a 256-entry table and
//! one Newton step, with UMULL. The clamps that keep texture reads inside
//! the surface are id's.

use std::ffi::c_int;

#[repr(C)]
pub struct Espan {
    u: c_int,
    v: c_int,
    count: c_int,
    pnext: *mut Espan,
}

// The engine's drawing state, which it sets per surface before a span
// drawer is called (d_edge.c's D_DrawSurfaces, D_CalcGradients). `static
// mut`, as the engine writes them between calls; only ever read here, by
// value.
unsafe extern "C" {
    static mut d_sdivzstepu: f32;
    static mut d_tdivzstepu: f32;
    static mut d_zistepu: f32;
    static mut d_sdivzstepv: f32;
    static mut d_tdivzstepv: f32;
    static mut d_zistepv: f32;
    static mut d_sdivzorigin: f32;
    static mut d_tdivzorigin: f32;
    static mut d_ziorigin: f32;
    static mut sadjust: c_int;
    static mut tadjust: c_int;
    static mut bbextents: c_int;
    static mut bbextentt: c_int;
    static mut cacheblock: *const u8;
    static mut cachewidth: c_int;
    static mut d_viewbuffer: *mut u8;
    static mut screenwidth: c_int;
    static mut d_pzbuffer: *mut i16;
    static mut d_zwidth: u32;
}

/// The power of two of |f| (f in [2^e, 2^(e+1))), from its bits; very
/// negative for zero.
#[inline(always)]
fn exponent(f: f32) -> i32 {
    ((f.to_bits() >> 23) & 0xff) as i32 - 127
}

/// f * 2^shift, truncated, from the bits: no soft-float call. The caller
/// keeps the result under 2^31.
#[inline(always)]
fn fix(f: f32, shift: i32) -> i32 {
    let bits = f.to_bits();
    let e = ((bits >> 23) & 0xff) as i32;
    if e == 0 {
        return 0;
    }
    let m = ((bits & 0x7f_ffff) | 0x80_0000) as i64;
    let sh = e - 127 - 23 + shift;
    let v = if sh >= 0 {
        if sh > 38 { i64::MAX >> 1 } else { m << sh }
    } else if sh < -40 {
        0
    } else {
        m >> -sh
    };
    let v = v.min(i32::MAX as i64) as i32;
    if bits >> 31 != 0 { -v } else { v }
}

/// 2^62 / x for x in [2^31, 2^32): table guess, one Newton step.
static RECIP: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        // x at the middle of the bucket, as a fraction in [0.5, 1).
        let x_num = 512 + 2 * i as u64 + 1; // x = x_num / 1024
        t[i] = ((1u64 << 40) / x_num) as u32; // 2^30 / x
        i += 1;
    }
    t
};

#[inline(always)]
fn recip(xn: u32) -> u32 {
    let y0 = RECIP[((xn >> 23) & 0xff) as usize] as u64; // Q2.30 of 2^32/xn
    let p = (xn as u64 * y0) >> 32; // x * y0, Q.30
    let two_minus = (1u64 << 31).saturating_sub(p);
    ((y0 * two_minus) >> 30).min(u32::MAX as u64) as u32
}

/// num / den * 2^extra, for den > 0 (fixed point, any common scale).
#[inline(always)]
fn divide(num: i32, den: i32, extra: i32) -> i32 {
    if den <= 0 {
        return if num >= 0 { i32::MAX } else { i32::MIN };
    }
    let n = (den as u32).leading_zeros() as i32;
    let y = recip((den as u32) << n) as i64; // 2^62 / (den << n)
    let shift = 62 - n - extra;
    let p = num as i64 * y;
    let q = if shift <= 0 {
        if p == 0 {
            0
        } else if p > 0 {
            i64::MAX
        } else {
            i64::MIN
        }
    } else if shift >= 63 {
        0
    } else {
        p >> shift
    };
    q.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Texture coordinate at one point of a span, 16.16, clamped like id's.
#[inline(always)]
fn coord(sz: i32, z: i32, extra: i32, adjust: c_int, lo: c_int, hi: c_int) -> c_int {
    divide(sz, z, extra).saturating_add(adjust).clamp(lo, hi)
}

/// The screen rectangle a list of spans covers: (umin, vmin, umax, vmax).
///
/// # Safety
/// `pspan` is null or the head of a list of valid espan_t, linked by pnext
/// and ended by a null, none written while this runs.
unsafe fn bounds(mut pspan: *const Espan) -> (i32, i32, i32, i32) {
    let (mut umin, mut vmin, mut umax, mut vmax) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    // SAFETY: each pspan dereferenced is non-null and a valid espan_t of
    // the list, as the caller promises.
    unsafe {
        while !pspan.is_null() {
            let sp = &*pspan;
            if sp.count > 0 {
                umin = umin.min(sp.u);
                umax = umax.max(sp.u + sp.count - 1);
                vmin = vmin.min(sp.v);
                vmax = vmax.max(sp.v);
            }
            pspan = sp.pnext;
        }
    }
    (umin, vmin, umax, vmax)
}

/// origin + v * stepv + u * stepu, in floats, at one screen point.
#[inline(always)]
fn at(origin: f32, stepu: f32, stepv: f32, u: i32, v: i32) -> f32 {
    origin + v as f32 * stepv + u as f32 * stepu
}

/// A value linear in screen position, in fixed point for one surface: its
/// scale k comes from the largest magnitude on the surface's rectangle,
/// which for a linear function is at a corner. So no span overflows, and
/// span starts, ends and steps are integer arithmetic.
struct Plane {
    k: i32,
    at: i32,
    du: i32,
    dv: i32,
    u0: i32,
    v0: i32,
}

impl Plane {
    fn new(origin: f32, stepu: f32, stepv: f32, (u0, v0, u1, v1): (i32, i32, i32, i32)) -> Plane {
        let c00 = at(origin, stepu, stepv, u0, v0);
        let c10 = at(origin, stepu, stepv, u1, v0);
        let c01 = at(origin, stepu, stepv, u0, v1);
        let c11 = at(origin, stepu, stepv, u1, v1);
        let e = exponent(c00).max(exponent(c10)).max(exponent(c01)).max(exponent(c11));
        // All four corners zero (or all but): any scale will do.
        let k = if e < -100 { 0 } else { (29 - e).min(60) };
        Plane { k, at: fix(c00, k), du: fix(stepu, k), dv: fix(stepv, k), u0, v0 }
    }

    #[inline(always)]
    fn value(&self, u: i32, v: i32) -> i32 {
        self.at.wrapping_add((u - self.u0).wrapping_mul(self.du)).wrapping_add((v - self.v0).wrapping_mul(self.dv))
    }
}

/// # Safety
/// As the engine calls it (d_edge.c, through d_drawspans): `pspan` is a
/// surface's list of spans, ended by a null, each on the view buffer
/// (d_viewbuffer, screenwidth bytes a row), and the d_* gradients,
/// sadjust/tadjust, bbextents/bbextentt and cacheblock/cachewidth are set
/// for that surface (D_CalcGradients, and its surface cache block).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn D_DrawSpans8(mut pspan: *mut Espan) {
    // SAFETY: the statics are read by value, on the engine's thread, which
    // doesn't write them during the call. The spans are a valid list (the
    // caller's promise), so bounds() and each &*pspan are sound. Each
    // span's pixels, u to u + count - 1 on row v, are on the view buffer,
    // so pdest and the count writes after it are in it. The texture reads
    // are in the cache block: s and t are clamped, as id's d_scan.c clamps
    // them, to 0..=bbextents and 0..=bbextentt (8.. at the steps, its
    // guard against round-off), each pixel's ss/tt lies between two clamped
    // ends, and the block is cachewidth bytes by (bbextentt >> 16) + 1 rows
    // (bbextents >> 16 < cachewidth).
    unsafe {
        let pbase = cacheblock;
        let cw = cachewidth as isize;
        let rect = bounds(pspan);
        if rect.0 > rect.2 {
            return;
        }
        // Per surface, the only floats: s/z, t/z and 1/z at the corners.
        let sp_ = Plane::new(d_sdivzorigin, d_sdivzstepu, d_sdivzstepv, rect);
        let tp = Plane::new(d_tdivzorigin, d_tdivzstepu, d_tdivzstepv, rect);
        let zp = Plane::new(d_ziorigin, d_zistepu, d_zistepv, rect);
        let (s_z8, t_z8, z8) = (sp_.du.wrapping_mul(8), tp.du.wrapping_mul(8), zp.du.wrapping_mul(8));
        // s = (s/z) / (1/z) * 2^16, from scales 2^ks and 2^kz.
        let (es, et) = (zp.k - sp_.k + 16, zp.k - tp.k + 16);
        while !pspan.is_null() {
            let sp = &*pspan;
            pspan = sp.pnext;
            let count = sp.count;
            if count <= 0 {
                continue;
            }
            let pdest = d_viewbuffer.offset((screenwidth * sp.v + sp.u) as isize);

            let last = sp.u + count - 1;
            let (mut s_z, mut t_z, mut z) = (sp_.value(sp.u, sp.v), tp.value(sp.u, sp.v), zp.value(sp.u, sp.v));
            let (s_end, t_end, z_end) = (sp_.value(last, sp.v), tp.value(last, sp.v), zp.value(last, sp.v));

            let mut s = coord(s_z, z, es, sadjust, 0, bbextents);
            let mut t = coord(t_z, z, et, tadjust, 0, bbextentt);

            let mut remaining = count;
            let mut dst = pdest;
            while remaining > 0 {
                let n = remaining.min(8);
                remaining -= n;
                let (snext, tnext, sstep, tstep);
                if remaining > 0 {
                    s_z = s_z.wrapping_add(s_z8);
                    t_z = t_z.wrapping_add(t_z8);
                    z = z.wrapping_add(z8);
                    // id's guard: at least 8 so round-off on <0 steps
                    // cannot run off the texture's edge.
                    snext = coord(s_z, z, es, sadjust, 8, bbextents);
                    tnext = coord(t_z, z, et, tadjust, 8, bbextentt);
                    sstep = (snext - s) >> 3;
                    tstep = (tnext - t) >> 3;
                } else {
                    // The span's last pixel, exactly.
                    snext = coord(s_end, z_end, es, sadjust, 8, bbextents);
                    tnext = coord(t_end, z_end, et, tadjust, 8, bbextentt);
                    if n > 1 {
                        sstep = (snext - s) / (n - 1);
                        tstep = (tnext - t) / (n - 1);
                    } else {
                        sstep = 0;
                        tstep = 0;
                    }
                }
                let (mut ss, mut tt) = (s, t);
                for _ in 0..n {
                    *dst = *pbase.offset((ss >> 16) as isize + (tt >> 16) as isize * cw);
                    dst = dst.add(1);
                    ss = ss.wrapping_add(sstep);
                    tt = tt.wrapping_add(tstep);
                }
                s = snext;
                t = tnext;
            }
        }
    }
}

/// D_DrawZSpans (d_scan.c): 1/z into the z-buffer, 16 bits a pixel. id's
/// works out each span's start in double precision; here the start is an
/// integer plane at id's scale (2^31), in 64 bits, saturated where id's
/// float-to-int conversion would have.
/// # Safety
/// As the engine calls it (d_edge.c): `pspan` is a list of spans, ended by a
/// null, each on the z-buffer (d_pzbuffer, d_zwidth values a row), and the
/// d_zi* gradients are set for the surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn D_DrawZSpans(mut pspan: *mut Espan) {
    // SAFETY: the statics are read by value, on the engine's thread. The
    // spans are a valid list (the caller's promise). Each span's count
    // values from u on row v are in the z-buffer, and the writes cover
    // exactly them: one i16 if dst isn't 4-aligned, then count / 2 u32s
    // (aligned now: d_pzbuffer is i16-aligned, so dst was 2 off at most),
    // then one i16 if one is left.
    unsafe {
        let rect = bounds(pspan);
        if rect.0 > rect.2 {
            return;
        }
        let izistep = fix(d_zistepu, 31);
        let at0 = fix64(at(d_ziorigin, d_zistepu, d_zistepv, rect.0, rect.1), 31);
        let (du, dv) = (fix64(d_zistepu, 31), fix64(d_zistepv, 31));
        while !pspan.is_null() {
            let sp = &*pspan;
            pspan = sp.pnext;
            let mut count = sp.count;
            if count <= 0 {
                continue;
            }
            let start = at0 + (sp.u - rect.0) as i64 * du + (sp.v - rect.1) as i64 * dv;
            let mut izi = start.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let mut dst = d_pzbuffer.offset((d_zwidth as i32 * sp.v + sp.u) as isize);
            // To a 4-byte boundary, then two pixels to a store.
            if (dst as usize) & 2 != 0 {
                *dst = (izi >> 16) as i16;
                dst = dst.add(1);
                izi = izi.wrapping_add(izistep);
                count -= 1;
            }
            let mut pair = dst as *mut u32;
            for _ in 0..count >> 1 {
                let lo = (izi as u32) >> 16;
                izi = izi.wrapping_add(izistep);
                *pair = lo | (izi as u32 & 0xffff_0000);
                izi = izi.wrapping_add(izistep);
                pair = pair.add(1);
            }
            if count & 1 != 0 {
                *(pair as *mut i16) = (izi >> 16) as i16;
            }
        }
    }
}

/// f * 2^shift, truncated, in 64 bits.
#[inline(always)]
fn fix64(f: f32, shift: i32) -> i64 {
    let bits = f.to_bits();
    let e = ((bits >> 23) & 0xff) as i32;
    if e == 0 {
        return 0;
    }
    let m = ((bits & 0x7f_ffff) | 0x80_0000) as i64;
    let sh = e - 127 - 23 + shift;
    let v = if sh >= 0 {
        m << sh.min(39)
    } else if sh < -40 {
        0
    } else {
        m >> -sh
    };
    if bits >> 31 != 0 { -v } else { v }
}
