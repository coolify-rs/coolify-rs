//! `cargo run -p coolify-compat --example encrypt -- <APP_KEY> <plaintext>`
//! Used by scripts/verify-encrypter-interop.sh.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let key = args.next().ok_or("missing APP_KEY")?;
    let plaintext = args.next().ok_or("missing plaintext")?;
    let enc = coolify_compat::Encrypter::new(&key)?;
    println!("{}", enc.encrypt_string(&plaintext));
    Ok(())
}
