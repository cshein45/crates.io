use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub iat: i64,
    pub exp: i64,
    pub iss: String,
}

/// Builds a short-lived JWT signed with the GitHub App's private key.
///
/// The `iat` claim is backdated by 60 seconds to tolerate clock skew,
/// and `exp` is set 9 minutes in the future (below GitHub's 10-minute
/// maximum). The `iss` claim carries the app's client id.
pub fn build_jwt(client_id: &str, key: &EncodingKey) -> anyhow::Result<SecretString> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        iat: now - 60,
        exp: now + 9 * 60,
        iss: client_id.to_string(),
    };

    let header = Header::new(Algorithm::RS256);
    let token = encode(&header, &claims, key)?;
    Ok(token.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_keys::{TEST_PRIVATE_KEY_PEM, TEST_PUBLIC_KEY_PEM};
    use claims::{assert_gt, assert_lt};
    use jsonwebtoken::{DecodingKey, Validation, decode};
    use secrecy::ExposeSecret;

    #[test]
    fn round_trip_claims() {
        let key = EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).unwrap();
        let token = build_jwt("Iv1.abc123", &key).unwrap();

        let decoding_key = DecodingKey::from_rsa_pem(TEST_PUBLIC_KEY_PEM.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = true;
        validation.required_spec_claims.clear();
        validation.required_spec_claims.insert("exp".to_string());

        let claims = decode::<Claims>(token.expose_secret(), &decoding_key, &validation)
            .unwrap()
            .claims;
        let now = Utc::now().timestamp();

        let threshold_sec = 5;
        assert_eq!(claims.iss, "Iv1.abc123");
        assert_gt!(claims.iat, now - 60 - threshold_sec);
        assert_lt!(claims.iat, now);
        assert_gt!(claims.exp, now);
        assert_lt!(claims.exp, now + 9 * 60 + threshold_sec);
    }
}
