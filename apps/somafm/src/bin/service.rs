//! neochumby-somafm-service: plays the station the screen chose, for the
//! whole session (player.rs).

fn main() {
    #[cfg(target_arch = "arm")]
    std::process::exit(neochumby_somafm::device::service());
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!("neochumby-somafm-service runs on the Chumby");
        std::process::exit(2);
    }
}
