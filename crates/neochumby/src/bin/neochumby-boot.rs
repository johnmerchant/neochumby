//! neochumby-boot: what the boot hook runs, from /psp, for a tree it
//! mounted over NFS (`noexec`, so nothing on it runs by itself). It checks
//! the tree's manifest against the key of the neochumbyd this Chumby
//! paired with, copies the session out of the tree into RAM, checked, and
//! runs that (neochumby::verified). On any failure it runs nothing from
//! the tree and exits 1.
//!
//!   neochumby-boot [ROOT]                   the tree, default /mnt/neochumby
//!   neochumby-boot --sideload ROOT FILE...  on the stock firmware: checked
//!                                           copies of the sideload's files
//!   neochumby-boot --update ROOT            on the stock firmware: the hook
//!                                           and this program in /psp, from
//!                                           the tree
//!
//! This program also runs on the stock Linux 2.6.16.

use std::path::{Path, PathBuf};

use neochumby::verified;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (what, root, result) = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--sideload", root, ..] => ("sideload", *root, verified::sideload(Path::new(root), &args[2..])),
        ["--update", root] => ("update", *root, verified::update(Path::new(root))),
        _ => {
            let root = args.first().map_or_else(|| PathBuf::from(verified::MNT), PathBuf::from);
            let e = verified::boot(&root);
            eprintln!("neochumby-boot: {}: {e}; nothing from it runs", root.display());
            std::process::exit(1)
        }
    };
    if let Err(e) = result {
        eprintln!("neochumby-boot: {what}: {root}: {e}");
        std::process::exit(1)
    }
}
