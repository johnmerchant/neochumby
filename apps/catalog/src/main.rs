//! neochumby-catalog, GET APPS: the catalog's screens (lib.rs), on the
//! terminal the session gives it.

fn main() {
    #[cfg(target_arch = "arm")]
    {
        neochumby::debug::install("neochumby-catalog");
        std::process::exit(neochumby_catalog::device::run());
    }
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!("neochumby-catalog: needs a Chumby");
        std::process::exit(2);
    }
}
