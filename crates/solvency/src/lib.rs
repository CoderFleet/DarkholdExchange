//! Merkle-sum solvency trees.
//!
//! One tree is built per asset. Every leaf commits to a user, the asset, and
//! that user's total balance (`available + held`). Internal nodes commit to the
//! sum of their children, so the root's sum is the total published liability for
//! the asset.
//!
//! The salt is deterministic for a snapshot/user/asset pair so the snapshot can
//! be rebuilt byte-for-byte, but it is published with the proof and is not a
//! secret. A proof demonstrates inclusion in the published liabilities; it does
//! not claim those reserves are backed by real funds.

use std::collections::BTreeMap;

pub use dkx_hash::PathDirection;

use dkx_hash::{
    EMPTY_NODE_HASH, EMPTY_NODE_SUM, MERKLE_ASSET_CASH, MERKLE_ASSET_PASS, MerkleNode,
    MerklePathStep, hash_merkle_leaf_with_asset, hash_merkle_node, pair_with_empty,
    verify_merkle_path,
};
use dx_types::{Asset, MarketId, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SOLVENCY_SALT_DOMAIN: &[u8] = b"DKX-SOL-SALT-v1";

#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum SolvencyError {
    #[error("duplicate user {0} in solvency tree")]
    DuplicateUser(UserId),

    #[error("user {user_id} is not in the {asset:?} solvency tree")]
    UserNotInTree { user_id: UserId, asset: Asset },

    #[error(transparent)]
    Hash(#[from] dkx_hash::HashError),
}

/// One user's total balance for one asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvencyLeaf {
    pub user_id: UserId,
    pub salt: [u8; 16],
    pub balance: u64,
}

/// A published root for one asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolvencyRoot {
    pub asset: Asset,
    pub root_hash: [u8; 32],
    pub total_liabilities: u64,
    pub leaf_count: usize,
}

/// A user's inclusion proof against one published asset root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolvencyProof {
    pub asset: Asset,
    pub snapshot_seq: u64,
    pub user_id: UserId,
    pub salt: [u8; 16],
    pub balance: u64,
    pub path: Vec<MerklePathStep>,
    pub root_hash: [u8; 32],
    pub total_liabilities: u64,
}

impl SolvencyProof {
    /// Recompute the leaf and walk the path to the published root.
    pub fn verify(&self) -> Result<bool, SolvencyError> {
        let leaf_hash = leaf_hash(self.asset, &self.salt, self.user_id, self.balance);
        verify_merkle_path(
            &leaf_hash,
            self.balance,
            &self.path,
            &self.root_hash,
            self.total_liabilities,
        )
        .map_err(SolvencyError::from)
    }

    /// The leaf hash a browser should display or compare while verifying.
    pub fn leaf_hash(&self) -> [u8; 32] {
        leaf_hash(self.asset, &self.salt, self.user_id, self.balance)
    }
}

/// A single asset's Merkle-sum tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvencyTree {
    asset: Asset,
    snapshot_seq: u64,
    leaves: Vec<SolvencyLeaf>,
    /// Bottom-up levels. Level 0 is leaves; the last level contains the root.
    levels: Vec<Vec<MerkleNode>>,
}

impl SolvencyTree {
    pub fn new(
        asset: Asset,
        snapshot_seq: u64,
        mut leaves: Vec<SolvencyLeaf>,
    ) -> Result<Self, SolvencyError> {
        leaves.sort_by_key(|leaf| leaf.user_id);
        for pair in leaves.windows(2) {
            if pair[0].user_id == pair[1].user_id {
                return Err(SolvencyError::DuplicateUser(pair[0].user_id));
            }
        }

        let levels = build_levels(asset, &leaves)?;
        Ok(Self {
            asset,
            snapshot_seq,
            leaves,
            levels,
        })
    }

    /// Build a tree and derive a deterministic salt for every user.
    pub fn from_balances(
        asset: Asset,
        snapshot_seq: u64,
        balances: impl IntoIterator<Item = (UserId, u64)>,
    ) -> Result<Self, SolvencyError> {
        let leaves = balances
            .into_iter()
            .map(|(user_id, balance)| SolvencyLeaf {
                user_id,
                salt: salt_for(snapshot_seq, asset, user_id),
                balance,
            })
            .collect();
        Self::new(asset, snapshot_seq, leaves)
    }

    pub fn asset(&self) -> Asset {
        self.asset
    }

    pub fn snapshot_seq(&self) -> u64 {
        self.snapshot_seq
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    pub fn total_liabilities(&self) -> u64 {
        self.root_node().sum
    }

    pub fn root_hash(&self) -> [u8; 32] {
        self.root_node().hash
    }

    pub fn root(&self) -> SolvencyRoot {
        SolvencyRoot {
            asset: self.asset,
            root_hash: self.root_hash(),
            total_liabilities: self.total_liabilities(),
            leaf_count: self.leaves.len(),
        }
    }

    pub fn proof(&self, user_id: UserId) -> Result<SolvencyProof, SolvencyError> {
        let leaf_index = self
            .leaves
            .binary_search_by_key(&user_id, |leaf| leaf.user_id)
            .map_err(|_| SolvencyError::UserNotInTree {
                user_id,
                asset: self.asset,
            })?;
        let leaf = &self.leaves[leaf_index];
        let height = self.levels.len().saturating_sub(1);
        let mut path = Vec::with_capacity(height);

        for level in 0..height {
            let index = leaf_index >> level;
            let sibling_index = index ^ 1;
            let (sibling_hash, sibling_sum, sibling_direction) =
                match self.levels[level].get(sibling_index) {
                    Some(sibling) => {
                        let direction = if sibling_index < index {
                            PathDirection::Left
                        } else {
                            PathDirection::Right
                        };
                        (sibling.hash, sibling.sum, direction)
                    }
                    // Odd last node: its sibling is the empty node on the right.
                    None => (EMPTY_NODE_HASH, EMPTY_NODE_SUM, PathDirection::Right),
                };
            path.push(MerklePathStep {
                sibling_hash,
                sibling_sum,
                sibling_direction,
            });
        }

        Ok(SolvencyProof {
            asset: self.asset,
            snapshot_seq: self.snapshot_seq,
            user_id: leaf.user_id,
            salt: leaf.salt,
            balance: leaf.balance,
            path,
            root_hash: self.root_hash(),
            total_liabilities: self.total_liabilities(),
        })
    }

    fn root_node(&self) -> MerkleNode {
        self.levels
            .last()
            .and_then(|level| level.first())
            .cloned()
            .unwrap_or(MerkleNode {
                hash: EMPTY_NODE_HASH,
                sum: EMPTY_NODE_SUM,
            })
    }
}

/// All per-asset trees for one snapshot sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolvencySnapshot {
    pub seq: u64,
    trees: BTreeMap<Asset, SolvencyTree>,
}

impl SolvencySnapshot {
    pub fn from_balances(
        seq: u64,
        balances: impl IntoIterator<Item = (UserId, Asset, u64)>,
    ) -> Result<Self, SolvencyError> {
        let mut grouped: BTreeMap<Asset, Vec<(UserId, u64)>> = BTreeMap::new();
        for (user_id, asset, balance) in balances {
            grouped.entry(asset).or_default().push((user_id, balance));
        }

        let trees = grouped
            .into_iter()
            .map(|(asset, balances)| {
                SolvencyTree::from_balances(asset, seq, balances).map(|tree| (asset, tree))
            })
            .collect::<Result<_, _>>()?;

        Ok(Self { seq, trees })
    }

    pub fn tree(&self, asset: Asset) -> Option<&SolvencyTree> {
        self.trees.get(&asset)
    }

    pub fn proof(&self, user_id: UserId, asset: Asset) -> Result<SolvencyProof, SolvencyError> {
        self.trees
            .get(&asset)
            .ok_or(SolvencyError::UserNotInTree { user_id, asset })?
            .proof(user_id)
    }

    pub fn roots(&self) -> Vec<SolvencyRoot> {
        self.trees.values().map(SolvencyTree::root).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }
}

/// Canonical asset tag and market id used inside a leaf.
pub fn encode_asset(asset: Asset) -> (u8, MarketId) {
    match asset {
        Asset::Cash => (MERKLE_ASSET_CASH, 0),
        Asset::Pass { market_id } => (MERKLE_ASSET_PASS, market_id),
    }
}

/// Deterministic public salt for one snapshot/user/asset.
pub fn salt_for(snapshot_seq: u64, asset: Asset, user_id: UserId) -> [u8; 16] {
    let (asset_kind, market_id) = encode_asset(asset);
    let mut hasher = Sha256::new();
    hasher.update(SOLVENCY_SALT_DOMAIN);
    hasher.update(snapshot_seq.to_be_bytes());
    hasher.update([asset_kind]);
    hasher.update(market_id.to_be_bytes());
    hasher.update(user_id.to_be_bytes());

    let digest = hasher.finalize();
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&digest[..16]);
    salt
}

fn leaf_hash(asset: Asset, salt: &[u8; 16], user_id: UserId, balance: u64) -> [u8; 32] {
    let (asset_kind, market_id) = encode_asset(asset);
    hash_merkle_leaf_with_asset(salt, user_id, asset_kind, market_id, balance)
}

fn build_levels(
    asset: Asset,
    leaves: &[SolvencyLeaf],
) -> Result<Vec<Vec<MerkleNode>>, SolvencyError> {
    if leaves.is_empty() {
        return Ok(vec![vec![MerkleNode {
            hash: EMPTY_NODE_HASH,
            sum: EMPTY_NODE_SUM,
        }]]);
    }

    let mut current: Vec<MerkleNode> = leaves
        .iter()
        .map(|leaf| MerkleNode {
            hash: leaf_hash(asset, &leaf.salt, leaf.user_id, leaf.balance),
            sum: leaf.balance,
        })
        .collect();
    let mut levels = vec![current.clone()];

    while current.len() > 1 {
        let mut next = Vec::with_capacity(current.len().div_ceil(2));
        for pair in current.chunks(2) {
            let node = if let Some(right) = pair.get(1) {
                hash_merkle_node(&pair[0].hash, pair[0].sum, &right.hash, right.sum)?
            } else {
                pair_with_empty(&pair[0])?
            };
            next.push(node);
        }
        levels.push(next.clone());
        current = next;
    }

    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKET: MarketId = 7;

    fn cash_balances() -> Vec<(UserId, u64)> {
        vec![(1, 100), (2, 200), (3, 300), (4, 400), (5, 500)]
    }

    #[test]
    fn empty_tree_has_the_zero_root() {
        let tree = SolvencyTree::from_balances(Asset::Cash, 1, []).unwrap();
        assert_eq!(tree.root_hash(), EMPTY_NODE_HASH);
        assert_eq!(tree.total_liabilities(), 0);
        assert_eq!(tree.leaf_count(), 0);
        assert_eq!(
            tree.proof(1),
            Err(SolvencyError::UserNotInTree {
                user_id: 1,
                asset: Asset::Cash,
            })
        );
    }

    #[test]
    fn single_leaf_is_its_own_root_and_has_an_empty_path() {
        let tree = SolvencyTree::from_balances(Asset::Cash, 9, [(42, 1_500_000)]).unwrap();
        let proof = tree.proof(42).unwrap();

        assert!(proof.path.is_empty());
        assert_eq!(proof.root_hash, tree.root_hash());
        assert_eq!(proof.total_liabilities, 1_500_000);
        assert!(proof.verify().unwrap());
    }

    #[test]
    fn every_inclusion_proof_verifies_and_sums_to_the_root() {
        let tree = SolvencyTree::from_balances(Asset::Cash, 1, cash_balances()).unwrap();
        assert_eq!(tree.total_liabilities(), 1_500);

        for (user_id, _) in cash_balances() {
            let proof = tree.proof(user_id).unwrap();
            assert_eq!(proof.root_hash, tree.root_hash());
            assert_eq!(proof.total_liabilities, tree.total_liabilities());
            assert!(proof.verify().unwrap(), "proof for user {user_id} failed");
        }
    }

    #[test]
    fn tampering_with_balance_root_or_path_fails() {
        let tree = SolvencyTree::from_balances(Asset::Cash, 1, cash_balances()).unwrap();
        let proof = tree.proof(3).unwrap();

        let mut tampered_balance = proof.clone();
        tampered_balance.balance += 1;
        assert!(!tampered_balance.verify().unwrap());

        let mut tampered_root = proof.clone();
        tampered_root.root_hash[0] ^= 1;
        assert!(!tampered_root.verify().unwrap());

        let mut tampered_path = proof;
        if let Some(step) = tampered_path.path.first_mut() {
            step.sibling_sum += 1;
        }
        assert!(!tampered_path.verify().unwrap());
    }

    #[test]
    fn duplicate_users_are_rejected() {
        let leaves = vec![
            SolvencyLeaf {
                user_id: 1,
                salt: [1; 16],
                balance: 10,
            },
            SolvencyLeaf {
                user_id: 1,
                salt: [2; 16],
                balance: 20,
            },
        ];
        assert_eq!(
            SolvencyTree::new(Asset::Cash, 1, leaves),
            Err(SolvencyError::DuplicateUser(1))
        );
    }

    #[test]
    fn insertion_order_does_not_change_the_root() {
        let mut forward = cash_balances();
        let mut reverse = forward.clone();
        reverse.reverse();

        let a = SolvencyTree::from_balances(Asset::Cash, 5, forward.drain(..)).unwrap();
        let b = SolvencyTree::from_balances(Asset::Cash, 5, reverse.drain(..)).unwrap();
        assert_eq!(a.root_hash(), b.root_hash());
        assert_eq!(a.total_liabilities(), b.total_liabilities());
    }

    #[test]
    fn assets_are_domain_separated() {
        let cash = SolvencyTree::from_balances(Asset::Cash, 1, cash_balances()).unwrap();
        let pass =
            SolvencyTree::from_balances(Asset::Pass { market_id: MARKET }, 1, cash_balances())
                .unwrap();
        assert_ne!(cash.root_hash(), pass.root_hash());
    }

    #[test]
    fn snapshot_builds_one_tree_per_asset() {
        let snapshot = SolvencySnapshot::from_balances(
            12,
            [
                (1, Asset::Cash, 1_000),
                (2, Asset::Cash, 2_000),
                (1, Asset::Pass { market_id: MARKET }, 3),
                (2, Asset::Pass { market_id: MARKET }, 4),
            ],
        )
        .unwrap();

        assert_eq!(snapshot.roots().len(), 2);
        assert_eq!(
            snapshot.tree(Asset::Cash).unwrap().total_liabilities(),
            3_000
        );
        assert_eq!(
            snapshot
                .tree(Asset::Pass { market_id: MARKET })
                .unwrap()
                .total_liabilities(),
            7
        );
        assert!(snapshot.proof(1, Asset::Cash).unwrap().verify().unwrap());
        assert!(snapshot.proof(1, Asset::Cash).unwrap().snapshot_seq == 12);
        assert_eq!(
            snapshot.proof(9, Asset::Cash),
            Err(SolvencyError::UserNotInTree {
                user_id: 9,
                asset: Asset::Cash,
            })
        );
    }
}
