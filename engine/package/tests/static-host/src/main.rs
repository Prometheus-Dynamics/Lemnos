//! Tries to load the package named by its first argument, by path and by scanning the
//! directory it is in, and prints what came of it.

#![forbid(unsafe_code)]

use std::path::Path;

use lemnos_package::{Loader, Mode, can_load};

fn main() {
    let path = std::env::args().nth(1).expect("a package's path");
    let path = Path::new(&path);
    println!("can load: {}", can_load());
    let mut loader = Loader::new(Mode::Open);
    match loader.load(path) {
        Ok(package) => println!("loaded: {}", package.name()),
        Err(why) => println!("refused: {why}"),
    }
    let scanned = loader.scan(path.parent().expect("a directory"));
    println!(
        "scan: {} loaded {} refused {}",
        if scanned.is_ok() {
            "read"
        } else {
            "unreadable"
        },
        loader.packages().len(),
        loader.refused().len()
    );
    for refused in loader.refused() {
        println!("scan refused: {}", refused.why);
    }
}
