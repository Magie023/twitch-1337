//! Hex-encoded double-submit CSRF token helpers.
//!
//! Verification uses constant-time comparison so an attacker cannot probe
//! per-byte differences via timing.

pub fn encode(value: &[u8; 32]) -> String {
    hex::encode(value)
}

pub fn decode(encoded: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(encoded).ok()?;
    bytes.try_into().ok()
}

pub fn verify(encoded: &str, expected: &[u8; 32]) -> bool {
    decode(encoded)
        .map(|got| constant_time_eq(&got, expected))
        .unwrap_or(false)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let value = [7u8; 32];
        let encoded = encode(&value);
        assert_eq!(encoded.len(), 64, "32 bytes hex-encode to 64 chars");
        assert_eq!(decode(&encoded), Some(value));
    }

    #[test]
    fn verify_accepts_matching_token() {
        let value = [0xABu8; 32];
        assert!(verify(&encode(&value), &value));
    }

    #[test]
    fn verify_rejects_single_byte_difference() {
        let expected = [0u8; 32];
        let mut tampered = expected;
        tampered[31] = 1;
        assert!(!verify(&encode(&tampered), &expected));
    }

    #[test]
    fn verify_rejects_non_hex() {
        assert!(!verify("nothex!!", &[0u8; 32]));
    }

    #[test]
    fn verify_rejects_wrong_length() {
        // 31 bytes => 62 hex chars: valid hex, wrong size for [u8; 32].
        let short = hex::encode([0u8; 31]);
        assert_eq!(decode(&short), None);
        assert!(!verify(&short, &[0u8; 32]));
        // 33 bytes => too long.
        let long = hex::encode([0u8; 33]);
        assert_eq!(decode(&long), None);
        assert!(!verify(&long, &[0u8; 32]));
    }

    #[test]
    fn constant_time_eq_matches_equality() {
        let a = [1u8; 32];
        let mut b = a;
        assert!(constant_time_eq(&a, &b));
        b[0] = 2;
        assert!(!constant_time_eq(&a, &b));
    }
}
