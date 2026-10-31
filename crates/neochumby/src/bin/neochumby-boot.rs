//! neochumby-boot: what the boot hook runs, from /psp, for a tree it
//! mounted over NFS (`noexec`, so nothing on it runs by itself). It checks
//! the tree's manifest against the key of the neochumbyd this Chumby
//! paired with, copies the session out of the tree into RAM, checked, and
//! runs that (neochumby::verified). On any failure it runs nothing from
//! the tree and exits 1, and the boot carries on with the Chumby's own
//! software.
//!
//!   neochumby-boot [ROOT]    the tree, default /mnt/neochumby

use std::path::PathBuf;

use neochumby::verified;

fn main() {
    let root = std::env::args_os().nth(1).map_or_else(|| PathBuf::from(verified::MNT), PathBuf::from);
    let e = verified::boot(&root);
    eprintln!("neochumby-boot: {}: {e}; nothing from it runs", root.display());
    std::process::exit(1)
}
