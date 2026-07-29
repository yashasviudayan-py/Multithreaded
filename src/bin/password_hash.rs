//! Generate an Argon2id password hash for `AUTH_PASSWORD_HASH`.

use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};

fn main() {
    let Some(password) = std::env::args().nth(1) else {
        eprintln!("Usage: cargo run --quiet --bin password_hash -- '<password>'");
        std::process::exit(2);
    };
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2id hashing should not fail");
    println!("{hash}");
}
