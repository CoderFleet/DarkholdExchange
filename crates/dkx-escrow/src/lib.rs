use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EscrowError {
    #[error("Missing DKX_SIGNING_KEY environment variable")]
    MissingSigningKey,

    #[error("Invalid DKX_SIGNING_KEY: must be 64 hex characters (32 bytes)")]
    InvalidSigningKey,

    #[error("Missing ESCROW_INSPECTION_SECS environment variable")]
    MissingInspectionSecs,

    #[error("Invalid ESCROW_INSPECTION_SECS")]
    InvalidInspectionSecs,

    #[error("Escrow not found: {0}")]
    NotFound(u64),

    #[error("Invalid state transition for escrow {id}: current state is {current:?}")]
    InvalidState { id: u64, current: EscrowState },

    #[error("Unauthorized: only buyer {expected} can perform this action, got {actual}")]
    UnauthorizedBuyer { expected: u64, actual: u64 },

    #[error("Unauthorized: only admin can perform this action")]
    UnauthorizedAdmin,

    #[error("Receipt signature verification failed")]
    InvalidSignature,

    #[error("Malformed receipt bytes")]
    MalformedReceiptBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EscrowState {
    Created,
    FundsLocked,
    Delivered,
    Released,
    Refunded,
    Disputed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminRuling {
    Buyer,
    Seller,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowRecord {
    pub id: u64,
    pub trade_id: u64,
    pub market_id: u32,
    pub buyer_id: u64,
    pub seller_id: u64,
    pub qty: u32,
    pub price: i64,
    pub fee: i64,
    pub state: EscrowState,
    pub delivered_at: u64,
    pub deadline: u64,
    pub receipt_bytes: Vec<u8>,
    pub receipt_sig: Vec<u8>,
    pub updated_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeRecord {
    pub id: u64,
    pub escrow_id: u64,
    pub reason: String,
    pub note: String,
    pub status: String,
    pub ruling: Option<AdminRuling>,
    pub ruled_by: Option<u64>,
    pub ruled_at: Option<u64>,
}

/// Constructs the canonical receipt byte sequence:
/// 'DKX-RCPT-v1' | escrow_id u64 | trade_id u64 | market u32 | buyer u64 | seller u64 | qty u32 | price i64 | delivered_at u64
#[allow(clippy::too_many_arguments)]
pub fn build_receipt_bytes(
    escrow_id: u64,
    trade_id: u64,
    market_id: u32,
    buyer_id: u64,
    seller_id: u64,
    qty: u32,
    price: i64,
    delivered_at: u64,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(66);
    bytes.extend_from_slice(b"DKX-RCPT-v1");
    bytes.extend_from_slice(&escrow_id.to_be_bytes());
    bytes.extend_from_slice(&trade_id.to_be_bytes());
    bytes.extend_from_slice(&market_id.to_be_bytes());
    bytes.extend_from_slice(&buyer_id.to_be_bytes());
    bytes.extend_from_slice(&seller_id.to_be_bytes());
    bytes.extend_from_slice(&qty.to_be_bytes());
    bytes.extend_from_slice(&price.to_be_bytes());
    bytes.extend_from_slice(&delivered_at.to_be_bytes());
    bytes
}

/// Signs receipt bytes using the Ed25519 platform signing key.
pub fn sign_receipt(signing_key: &SigningKey, receipt_bytes: &[u8]) -> Vec<u8> {
    let signature: Signature = signing_key.sign(receipt_bytes);
    signature.to_bytes().to_vec()
}

/// Verifies a receipt signature against the platform public key.
pub fn verify_receipt(
    verifying_key: &VerifyingKey,
    receipt_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<bool, EscrowError> {
    if receipt_bytes.len() != 66 || !receipt_bytes.starts_with(b"DKX-RCPT-v1") {
        return Err(EscrowError::MalformedReceiptBytes);
    }
    let sig_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| EscrowError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_array);
    verifying_key
        .verify(receipt_bytes, &signature)
        .map(|_| true)
        .map_err(|_| EscrowError::InvalidSignature)
}

/// Reads DKX_SIGNING_KEY from environment or given str. Must be 64 hex characters (32 bytes).
pub fn parse_signing_key(hex_str: &str) -> Result<SigningKey, EscrowError> {
    let bytes = hex::decode(hex_str.trim()).map_err(|_| EscrowError::InvalidSigningKey)?;
    if bytes.len() != 32 {
        return Err(EscrowError::InvalidSigningKey);
    }
    let key_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| EscrowError::InvalidSigningKey)?;
    Ok(SigningKey::from_bytes(&key_bytes))
}

/// Reads and parses ESCROW_INSPECTION_SECS from environment. Refuses to start if missing or invalid.
pub fn parse_inspection_secs_from_env() -> Result<u64, EscrowError> {
    let val =
        std::env::var("ESCROW_INSPECTION_SECS").map_err(|_| EscrowError::MissingInspectionSecs)?;
    val.parse::<u64>()
        .map_err(|_| EscrowError::InvalidInspectionSecs)
}

/// Loads the platform signing key from DKX_SIGNING_KEY env variable.
pub fn load_signing_key_from_env() -> Result<SigningKey, EscrowError> {
    let key_hex = std::env::var("DKX_SIGNING_KEY").map_err(|_| EscrowError::MissingSigningKey)?;
    parse_signing_key(&key_hex)
}
