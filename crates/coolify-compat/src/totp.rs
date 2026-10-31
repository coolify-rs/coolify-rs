//! TOTP verification compatible with Fortify's Google2FA provider.
//!
//! Fortify stores `two_factor_secret = encrypt(base32_secret)` and verifies
//! 6-digit, 30-second, HMAC-SHA1 codes with a window of one step on either
//! side (`verifyKeyNewer`), refusing a code whose step was already used.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;

const STEP: u64 = 30;
const DIGITS: u32 = 6;

/// RFC 4648 base32 (no padding required, case-insensitive), as Google2FA uses.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for c in s.trim().trim_end_matches('=').chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            ' ' | '-' => continue,
            _ => return None,
        };
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The code for a time step.
pub fn code_at(secret: &[u8], step: u64) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret)
        .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[h.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(h[offset] & 0x7f) << 24)
        | (u32::from(h[offset + 1]) << 16)
        | (u32::from(h[offset + 2]) << 8)
        | u32::from(h[offset + 3]);
    format!(
        "{:0width$}",
        bin % 10u32.pow(DIGITS),
        width = DIGITS as usize
    )
}

/// Verifies `code` at `unix_time` within `window` steps. Returns the matched
/// step, which must be newer than `last_used_step` (replay protection).
pub fn verify(
    secret_b32: &str,
    code: &str,
    unix_time: u64,
    window: u64,
    last_used_step: Option<u64>,
) -> Option<u64> {
    let secret = base32_decode(secret_b32)?;
    let code = code.trim().replace(' ', "");
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = unix_time / STEP;
    let mut matched = None;
    // Check every candidate so timing does not reveal which step matched.
    for step in now.saturating_sub(window)..=now + window {
        let ok = bool::from(code_at(&secret, step).as_bytes().ct_eq(code.as_bytes()));
        if ok && last_used_step.is_none_or(|last| step > last) {
            matched = Some(step);
        }
    }
    matched
}

/// `generateSecretKey`: a random base32 secret.
pub fn generate_secret(length: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// `RecoveryCode::generate`: `Str::random(10).'-'.Str::random(10)`.
pub fn recovery_code() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    let mut part = || -> String {
        (0..10)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
            .collect()
    };
    format!("{}-{}", part(), part())
}

/// `rawurlencode`.
fn raw_url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `QRCode::getQRCodeUrl` with Google2FA's defaults.
pub fn otpauth_url(company: &str, holder: &str, secret: &str) -> String {
    let company = raw_url_encode(company);
    format!(
        "otpauth://totp/{company}:{}?secret={secret}&issuer={company}&algorithm=SHA1&digits=6&period=30",
        raw_url_encode(holder)
    )
}

/// `twoFactorQrCodeSvg`: 192 pixels, no margin, a white ground and #2d3748 modules.
pub fn qr_svg(text: &str) -> String {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L) else {
        return String::new();
    };
    let width = code.width();
    let mut path = String::new();
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            path.push_str(&format!("M{} {}h1v1h-1Z", i % width, i / width));
        }
    }
    // The modules fill the whole image, as BaconQrCode's fractional module size does.
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"192\" height=\"192\" viewBox=\"0 0 {width} {width}\"><rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{width}\" fill=\"#ffffff\"/><path fill=\"#2d3748\" d=\"{path}\"/></svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 test secret "12345678901234567890" (ASCII), SHA-1, 8-digit
    // vectors truncated to 6 digits.
    const RFC_SECRET_B32: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn enrolment_values() {
        let secret = generate_secret(16);
        assert_eq!(secret.len(), 16);
        assert!(base32_decode(&secret).is_some());
        assert_eq!(recovery_code().len(), 21);
        let url = otpauth_url("Coolify", "a+b@x.test", "ABC");
        assert_eq!(
            url,
            "otpauth://totp/Coolify:a%2Bb%40x.test?secret=ABC&issuer=Coolify&algorithm=SHA1&digits=6&period=30"
        );
        assert!(qr_svg(&url).starts_with("<svg"));
    }

    #[test]
    fn rfc6238_vectors() {
        let s = base32_decode(RFC_SECRET_B32).unwrap();
        assert_eq!(s, b"12345678901234567890");
        assert_eq!(code_at(&s, 59 / 30), "287082");
        assert_eq!(code_at(&s, 1111111109 / 30), "081804");
        assert_eq!(code_at(&s, 1234567890 / 30), "005924");
        assert_eq!(code_at(&s, 2000000000 / 30), "279037");
    }

    #[test]
    fn window_and_replay() {
        let s = base32_decode(RFC_SECRET_B32).unwrap();
        let t = 1_234_567_890u64;
        let prev = code_at(&s, t / 30 - 1);
        assert_eq!(verify(RFC_SECRET_B32, &prev, t, 1, None), Some(t / 30 - 1));
        assert_eq!(verify(RFC_SECRET_B32, &prev, t, 0, None), None);
        let now = code_at(&s, t / 30);
        // Already used step: refused.
        assert_eq!(verify(RFC_SECRET_B32, &now, t, 1, Some(t / 30)), None);
        assert_eq!(verify(RFC_SECRET_B32, "12345", t, 1, None), None);
        assert_eq!(verify(RFC_SECRET_B32, "abcdef", t, 1, None), None);
    }
}
