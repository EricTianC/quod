//! Ed25519 actor request signing, verification, and RBAC types for Quod.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const HEADER_ACTOR_ID: &str = "x-quod-actor-id";
pub const HEADER_TIMESTAMP: &str = "x-quod-timestamp";
pub const HEADER_SIGNATURE: &str = "x-quod-signature";
pub const HEADER_ADMIN_TOKEN: &str = "x-quod-admin-token";

/// RBAC role granted to an isomorphic Quod `Actor` (human or external agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Orchestrator,
    Blueprinter,
    Auditor,
    Admin,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Worker => f.write_str("worker"),
            Self::Orchestrator => f.write_str("orchestrator"),
            Self::Blueprinter => f.write_str("blueprinter"),
            Self::Auditor => f.write_str("auditor"),
            Self::Admin => f.write_str("admin"),
        }
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "worker" => Ok(Self::Worker),
            "orchestrator" => Ok(Self::Orchestrator),
            "blueprinter" => Ok(Self::Blueprinter),
            "auditor" => Ok(Self::Auditor),
            "admin" => Ok(Self::Admin),
            other => Err(format!("unknown role '{other}'")),
        }
    }
}

/// Authenticated actor authorization context loaded from `actors`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorAuthContext {
    pub actor_id: String,
    pub roles: Vec<Role>,
    pub project_scopes: Vec<String>,
    pub enabled: bool,
}

impl ActorAuthContext {
    pub fn has_role(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    pub fn has_any_role(&self, allowed: &[Role]) -> bool {
        allowed.iter().any(|r| self.roles.contains(r))
    }

    /// Empty `project_scopes` grants access to all projects.
    pub fn can_access_project(&self, project_id: &str) -> bool {
        self.project_scopes.is_empty() || self.project_scopes.iter().any(|p| p == project_id)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthError {
    #[error("invalid Ed25519 key encoding: {0}")]
    InvalidKey(String),
    #[error("invalid Ed25519 signature encoding: {0}")]
    InvalidSignature(String),
    #[error("request timestamp skew too large (ts={timestamp_secs}, now={now_secs})")]
    TimestampSkew { timestamp_secs: i64, now_secs: i64 },
    #[error("Ed25519 signature verification failed")]
    VerificationFailed,
}

/// Generates a fresh `(secret_hex, public_hex)` Ed25519 keypair.
pub fn generate_keypair_hex() -> (String, String) {
    let signing = SigningKey::generate(&mut OsRng);
    let verifying = signing.verifying_key();
    (
        hex::encode(signing.to_bytes()),
        hex::encode(verifying.to_bytes()),
    )
}

/// Derives the hex-encoded public key from a 32-byte hex-encoded signing key.
pub fn public_key_from_secret_hex(secret_hex: &str) -> Result<String, AuthError> {
    let signing = decode_signing_key(secret_hex)?;
    Ok(hex::encode(signing.verifying_key().to_bytes()))
}

/// Validates that a hex string decodes to a valid 32-byte Ed25519 verifying key.
pub fn validate_pubkey_hex(public_hex: &str) -> Result<(), AuthError> {
    decode_verifying_key(public_hex).map(|_| ())
}

/// Canonical signing payload: `METHOD\nPATH\nTIMESTAMP\nSHA256_HEX(BODY)`.
pub fn canonical_signing_payload(
    method: &str,
    path: &str,
    timestamp_secs: i64,
    body: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let body_hash = hex::encode(hasher.finalize());
    format!(
        "{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path,
        timestamp_secs,
        body_hash
    )
}

/// Signs an HTTP request using a hex-encoded 32-byte Ed25519 secret key.
pub fn sign_request(
    secret_hex: &str,
    method: &str,
    path: &str,
    timestamp_secs: i64,
    body: &[u8],
) -> Result<String, AuthError> {
    let signing = decode_signing_key(secret_hex)?;
    let payload = canonical_signing_payload(method, path, timestamp_secs, body);
    let sig: Signature = signing.sign(payload.as_bytes());
    Ok(hex::encode(sig.to_bytes()))
}

/// Verifies an incoming HTTP request signature against a registered public key.
pub fn verify_request(
    public_hex: &str,
    method: &str,
    path: &str,
    timestamp_secs: i64,
    now_secs: i64,
    max_skew_secs: i64,
    body: &[u8],
    sig_hex: &str,
) -> Result<(), AuthError> {
    if (now_secs - timestamp_secs).abs() > max_skew_secs {
        return Err(AuthError::TimestampSkew {
            timestamp_secs,
            now_secs,
        });
    }
    let verifying = decode_verifying_key(public_hex)?;
    let sig = decode_signature(sig_hex)?;
    let payload = canonical_signing_payload(method, path, timestamp_secs, body);
    verifying
        .verify(payload.as_bytes(), &sig)
        .map_err(|_| AuthError::VerificationFailed)
}

fn decode_signing_key(secret_hex: &str) -> Result<SigningKey, AuthError> {
    let raw = hex::decode(secret_hex.trim())
        .map_err(|e| AuthError::InvalidKey(format!("bad hex: {e}")))?;
    let bytes: [u8; 32] = raw
        .try_into()
        .map_err(|_| AuthError::InvalidKey("expected 32-byte secret key".into()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn decode_verifying_key(public_hex: &str) -> Result<VerifyingKey, AuthError> {
    let raw = hex::decode(public_hex.trim())
        .map_err(|e| AuthError::InvalidKey(format!("bad hex: {e}")))?;
    let bytes: [u8; 32] = raw
        .try_into()
        .map_err(|_| AuthError::InvalidKey("expected 32-byte public key".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| AuthError::InvalidKey(format!("invalid curve point: {e}")))
}

fn decode_signature(sig_hex: &str) -> Result<Signature, AuthError> {
    let raw = hex::decode(sig_hex.trim())
        .map_err(|e| AuthError::InvalidSignature(format!("bad hex: {e}")))?;
    let bytes: [u8; 64] = raw
        .try_into()
        .map_err(|_| AuthError::InvalidSignature("expected 64-byte signature".into()))?;
    Ok(Signature::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip_and_rbac_helpers() {
        let (sk, pk) = generate_keypair_hex();
        assert_eq!(public_key_from_secret_hex(&sk).unwrap(), pk);

        let body = br#"{"task_id":"t1"}"#;
        let sig = sign_request(&sk, "POST", "/api/projects/p1/leases/claim", 1_700_000_000, body)
            .unwrap();
        verify_request(
            &pk,
            "POST",
            "/api/projects/p1/leases/claim",
            1_700_000_000,
            1_700_000_010,
            60,
            body,
            &sig,
        )
        .unwrap();

        assert_eq!(
            verify_request(
                &pk,
                "POST",
                "/api/projects/p1/leases/claim",
                1_700_000_000,
                1_700_000_010,
                60,
                br#"{"task_id":"t2"}"#,
                &sig,
            ),
            Err(AuthError::VerificationFailed)
        );

        assert!(matches!(
            verify_request(
                &pk,
                "POST",
                "/api/projects/p1/leases/claim",
                1_700_000_000,
                1_700_000_500,
                60,
                body,
                &sig,
            ),
            Err(AuthError::TimestampSkew { .. })
        ));

        let ctx = ActorAuthContext {
            actor_id: "orch-1".into(),
            roles: vec![Role::Orchestrator, Role::Worker],
            project_scopes: vec!["erdos-479".into()],
            enabled: true,
        };
        assert!(ctx.has_role(Role::Orchestrator));
        assert!(!ctx.has_role(Role::Auditor));
        assert!(ctx.can_access_project("erdos-479"));
        assert!(!ctx.can_access_project("other"));
    }
}
