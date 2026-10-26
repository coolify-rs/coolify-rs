//! Compatibility primitives shared with upstream Coolify (Laravel).
//!
//! Everything in this crate reproduces a storage or wire format that an
//! existing Coolify installation already depends on, so that coolify-rs can
//! adopt an upstream database without rewriting it.

pub mod encrypter;
pub mod php_json;
pub mod php_serialize;
pub mod sanctum;

pub use encrypter::{DecryptError, Encrypter, EncrypterKeyError};
pub use sanctum::{ParsedToken, hash_token, parse_bearer_token};
