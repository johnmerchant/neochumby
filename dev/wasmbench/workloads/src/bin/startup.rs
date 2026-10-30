//! startup: a main that does nothing, in a module of an app's size. Its
//! wall time is how long a runtime takes to load, validate (or compile) and
//! instantiate that much code, and call into it.

use workloads::startup::TABLE;

fn main() {
    // Never true, but the compiler can't know that, so every function in
    // the table stays in the module.
    let never = std::hint::black_box(0u32);
    if never != 0 {
        let mut m = [0u32; 16];
        std::process::exit(TABLE[never as usize % TABLE.len()](never, &mut m) as i32);
    }
}
