//! The feature baseline of the shipped programs. It has no code: its manifest (`Cargo.toml`) is the
//! point. Every shipped program depends on it, so each program requests the same features of the
//! crates they share, whether it is built alone or with the others.
