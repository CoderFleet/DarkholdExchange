use crate::error::HashError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MERKLE_LEAF_PREFIX: u8 = 0x00;
pub const MERKLE_NODE_PREFIX: u8 = 0x01;

/// Asset tag used by the asset-aware solvency leaf.
pub const MERKLE_ASSET_CASH: u8 = 0;
/// Asset tag used by the asset-aware solvency leaf.
pub const MERKLE_ASSET_PASS: u8 = 1;

/// Empty node constants: 32 zero bytes hash, sum 0
pub const EMPTY_NODE_HASH: [u8; 32] = [0u8; 32];
pub const EMPTY_NODE_SUM: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleNode {
    pub hash: [u8; 32],
    pub sum: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerklePathStep {
    pub sibling_hash: [u8; 32],
    pub sibling_sum: u64,
    pub sibling_direction: PathDirection,
}

/// Compute Merkle leaf hash:
/// SHA256( 0x00 | salt 16B | user_id u64 | balance u64 )
/// All integers big-endian.
pub fn hash_merkle_leaf(salt: &[u8; 16], user_id: u64, balance: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([MERKLE_LEAF_PREFIX]);
    hasher.update(salt);
    hasher.update(user_id.to_be_bytes());
    hasher.update(balance.to_be_bytes());

    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output);
    hash
}

/// Compute an asset-aware Merkle leaf hash:
/// SHA256( 0x00 | salt 16B | user_id u64 | asset_kind u8 | market_id u32 | balance u64 )
///
/// `asset_kind` is [`MERKLE_ASSET_CASH`] or [`MERKLE_ASSET_PASS`].
/// `market_id` is zero for cash. All integers big-endian.
pub fn hash_merkle_leaf_with_asset(
    salt: &[u8; 16],
    user_id: u64,
    asset_kind: u8,
    market_id: u32,
    balance: u64,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([MERKLE_LEAF_PREFIX]);
    hasher.update(salt);
    hasher.update(user_id.to_be_bytes());
    hasher.update([asset_kind]);
    hasher.update(market_id.to_be_bytes());
    hasher.update(balance.to_be_bytes());

    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output);
    hash
}

/// Compute Merkle parent node:
/// Node = SHA256( 0x01 | left_hash 32B | left_sum u64 | right_hash 32B | right_sum u64 )
/// node sum = left_sum + right_sum (check overflow).
/// All integers big-endian.
pub fn hash_merkle_node(
    left_hash: &[u8; 32],
    left_sum: u64,
    right_hash: &[u8; 32],
    right_sum: u64,
) -> Result<MerkleNode, HashError> {
    let sum = match left_sum.checked_add(right_sum) {
        Some(s) => s,
        None => return Err(HashError::SumOverflow),
    };

    let mut hasher = Sha256::new();
    hasher.update([MERKLE_NODE_PREFIX]);
    hasher.update(left_hash);
    hasher.update(left_sum.to_be_bytes());
    hasher.update(right_hash);
    hasher.update(right_sum.to_be_bytes());

    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&output);

    Ok(MerkleNode { hash, sum })
}

/// Pair odd-count elements by pairing the last node with the empty node (zero hash, sum 0).
pub fn pair_with_empty(node: &MerkleNode) -> Result<MerkleNode, HashError> {
    hash_merkle_node(&node.hash, node.sum, &EMPTY_NODE_HASH, EMPTY_NODE_SUM)
}

/// Verifies a Merkle inclusion path up to the root hash and root total sum.
pub fn verify_merkle_path(
    leaf_hash: &[u8; 32],
    leaf_balance: u64,
    path: &[MerklePathStep],
    expected_root_hash: &[u8; 32],
    expected_root_sum: u64,
) -> Result<bool, HashError> {
    let mut current_hash = *leaf_hash;
    let mut current_sum = leaf_balance;

    for step in path {
        let parent = match step.sibling_direction {
            // Sibling is to the left of the current node
            PathDirection::Left => hash_merkle_node(
                &step.sibling_hash,
                step.sibling_sum,
                &current_hash,
                current_sum,
            )?,
            // Sibling is to the right of the current node
            PathDirection::Right => hash_merkle_node(
                &current_hash,
                current_sum,
                &step.sibling_hash,
                step.sibling_sum,
            )?,
        };

        current_hash = parent.hash;
        current_sum = parent.sum;
    }

    Ok(current_hash == *expected_root_hash && current_sum == expected_root_sum)
}
