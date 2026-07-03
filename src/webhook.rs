use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

pub fn validate_gitlab_token(header_value: &str, secret: &str) -> bool {
    let header_bytes = header_value.as_bytes();
    let secret_bytes = secret.as_bytes();
    constant_time_eq(header_bytes, secret_bytes)
}

pub fn validate_github_signature(header: &str, body: &[u8], secret: &str) -> bool {
    let expected_prefix = "sha256=";
    if header.len() < expected_prefix.len() || !header.starts_with(expected_prefix) {
        return false;
    }

    let signature_hex = &header[expected_prefix.len()..];
    let expected = match hex::decode(signature_hex) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = mac.finalize().into_bytes();

    constant_time_eq(&computed, &expected)
}

pub fn validate_bitbucket_signature(header: &str, body: &[u8], secret: &str) -> bool {
    let expected_prefix = "sha256=";
    if header.len() < expected_prefix.len() || !header.starts_with(expected_prefix) {
        return false;
    }

    let signature_hex = &header[expected_prefix.len()..];
    let expected = match hex::decode(signature_hex) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = mac.finalize().into_bytes();

    constant_time_eq(&computed, &expected)
}

#[cfg(test)]
pub fn compute_hmac_sha256(secret: &str, body: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(body);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gitlab_token_validation_success() {
        let secret = "my-webhook-secret";
        let header_value = "my-webhook-secret";
        assert!(validate_gitlab_token(header_value, secret));
    }

    #[test]
    fn test_gitlab_token_validation_failure() {
        let secret = "my-webhook-secret";
        let header_value = "wrong-token";
        assert!(!validate_gitlab_token(header_value, secret));
    }

    #[test]
    fn test_gitlab_token_empty_secret() {
        assert!(!validate_gitlab_token("anything", ""));
        assert!(!validate_gitlab_token("", "secret"));
    }

    #[test]
    fn test_github_hmac_sha256_validation_success() {
        let secret = "my-secret";
        let body = b"payload-body";
        let signature = compute_hmac_sha256(secret, body);
        let header = format!("sha256={}", hex::encode(&signature));
        assert!(validate_github_signature(&header, body, secret));
    }

    #[test]
    fn test_github_hmac_sha256_validation_failure() {
        let secret = "my-secret";
        let body = b"payload-body";
        let header = "sha256=0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!validate_github_signature(header, body, secret));
    }

    #[test]
    fn test_github_validation_rejects_malformed_header() {
        let secret = "my-secret";
        let body = b"payload-body";
        assert!(!validate_github_signature(
            "not-a-valid-header",
            body,
            secret
        ));
        assert!(!validate_github_signature("sha256=", body, secret));
        assert!(!validate_github_signature("sha256=nothex", body, secret));
    }

    #[test]
    fn test_bitbucket_hmac_sha256_validation_success() {
        let secret = "bb-secret";
        let body = b"bitbucket-payload";
        let signature = compute_hmac_sha256(secret, body);
        let header = format!("sha256={}", hex::encode(&signature));
        assert!(validate_bitbucket_signature(&header, body, secret));
    }

    #[test]
    fn test_bitbucket_hmac_sha256_validation_failure() {
        let secret = "bb-secret";
        let body = b"bitbucket-payload";
        let header = "sha256=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert!(!validate_bitbucket_signature(header, body, secret));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hello!"));
    }
}
