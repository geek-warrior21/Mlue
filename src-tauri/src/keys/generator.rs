use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const KEY_PREFIX: &str = "mlue_sk_";
pub const SECRET_ENTROPY_BYTES: usize = 32; // 256 bits of entropy

pub struct GeneratedKey {
    pub full_key: String,
    pub key_prefix: String,
    pub key_hash: String,
}

/// Generates a cryptographically secure API key using the OS CSPRNG.
/// Format: mlue_sk_<base64_url_safe_32_bytes>
pub fn generate_api_key() -> GeneratedKey {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let mut random_bytes = [0u8; SECRET_ENTROPY_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut random_bytes);

    let secret_encoded = URL_SAFE_NO_PAD.encode(random_bytes);
    let full_key = format!("{}{}", KEY_PREFIX, secret_encoded);

    // Take "mlue_sk_" + 8 characters of secret for the prefix display
    let prefix_length = KEY_PREFIX.len() + 8;
    let key_prefix = if full_key.len() >= prefix_length {
        format!("{}...", &full_key[..prefix_length])
    } else {
        format!("{}...", KEY_PREFIX)
    };

    let key_hash = hash_api_key(&full_key);

    GeneratedKey {
        full_key,
        key_prefix,
        key_hash,
    }
}

/// Computes the SHA-256 digest of an API key string, returned as lowercase hex.
pub fn hash_api_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Constant-time comparison of two key hashes to prevent timing attacks.
pub fn verify_hash(candidate_hash: &str, expected_hash: &str) -> bool {
    candidate_hash
        .as_bytes()
        .ct_eq(expected_hash.as_bytes())
        .into()
}

/// Validates that a key follows the mlue_sk_ format and contains sufficient characters.
pub fn validate_key_format(raw_key: &str) -> bool {
    if !raw_key.starts_with(KEY_PREFIX) {
        return false;
    }
    let secret = &raw_key[KEY_PREFIX.len()..];
    // 32 bytes in URL_SAFE_NO_PAD base64 produces 43 characters
    secret.len() >= 40
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
