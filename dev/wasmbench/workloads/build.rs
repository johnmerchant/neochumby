//! Writes the `startup` workload's functions: STARTUP_FUNCS of them (289 by
//! default), each a little different and calling the one before, and a
//! table of them all that the binary can reach but doesn't call, so that
//! they stay in the module and make it about 200 KB as wasm.

use std::fmt::Write;

fn main() {
    println!("cargo::rerun-if-env-changed=STARTUP_FUNCS");
    let n: u64 = std::env::var("STARTUP_FUNCS").ok().and_then(|v| v.parse().ok()).unwrap_or(289);
    let mut s = String::new();
    for i in 0..n {
        // A little of everything: arithmetic, a loop, a branch, a load and
        // store, and a call to the previous function.
        let (mul, xor, sh, bit) = (i * 2654435761 % (1 << 32), i * 40503 % 65536, i % 13 + 1, 1u32 << (i % 31));
        let call = if i == 0 {
            String::new()
        } else {
            format!(".wrapping_add(if a > {i} {{ f{}(a - 1, m) }} else {{ 0 }})", i - 1)
        };
        write!(
            s,
            "
/// One of the functions that make up the module.
#[inline(never)]
pub fn f{i}(a: u32, m: &mut [u32; 16]) -> u32 {{
    let mut s = a.wrapping_mul({mul}).wrapping_add({i});
    for k in 0..a & 7 {{
        s ^= m[(s.wrapping_add(k) & 15) as usize].wrapping_add({xor});
        s = s.rotate_left({sh});
        if s & {bit} != 0 {{
            m[(k & 15) as usize] = m[(k & 15) as usize].wrapping_add(s);
        }} else {{
            s = s.wrapping_sub(m[{}]);
        }}
    }}
    s{call}
}}
",
            i % 16,
        )
        .unwrap();
    }
    let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
    write!(
        s,
        "\n/// Every function, by number.\npub static TABLE: [fn(u32, &mut [u32; 16]) -> u32; {n}] = [{}];\n",
        names.join(", ")
    )
    .unwrap();
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("startup.rs");
    std::fs::write(out, s).unwrap();
}
