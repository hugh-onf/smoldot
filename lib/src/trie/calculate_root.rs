// Smoldot
// Copyright (C) 2019-2022  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Freestanding function that calculates the root of a radix-16 Merkle-Patricia trie.
//!
//! See the parent module documentation for an explanation of what the trie is.
//!
//! # Usage
//!
//! Calling the [`root_merkle_value`] function creates a [`RootMerkleValueCalculation`] object
//! which you have to drive to completion.
//!
//! Example:
//!
//! ```
//! use std::collections::BTreeMap;
//! use smoldot::trie::{TrieEntryVersion, calculate_root};
//!
//! // In this example, the storage consists in a binary tree map.
//! let mut storage = BTreeMap::<Vec<u8>, (Vec<u8>, TrieEntryVersion)>::new();
//! storage.insert(b"foo".to_vec(), (b"bar".to_vec(), TrieEntryVersion::V1));
//!
//! let trie_root = {
//!     let mut calculation = calculate_root::root_merkle_value(None);
//!     loop {
//!         match calculation {
//!             calculate_root::RootMerkleValueCalculation::Finished { hash, .. } => break hash,
//!             calculate_root::RootMerkleValueCalculation::AllKeys(keys) => {
//!                 calculation = keys.inject(storage.keys().map(|k| k.iter().cloned()));
//!             }
//!             calculate_root::RootMerkleValueCalculation::StorageValue(value_request) => {
//!                 let key = value_request.key().collect::<Vec<u8>>();
//!                 calculation = value_request.inject(storage.get(&key).map(|(val, v)| (val, *v)));
//!             }
//!         }
//!     }
//! };
//!
//! assert_eq!(
//!     trie_root,
//!     [204, 86, 28, 213, 155, 206, 247, 145, 28, 169, 212, 146, 182, 159, 224, 82,
//!      116, 162, 143, 156, 19, 43, 183, 8, 41, 178, 204, 69, 41, 37, 224, 91]
//! );
//! ```
//!
//! You have the possibility to pass a [`CalculationCache`] to the calculation. This cache will
//! be filled with intermediary calculations and can later be passed again to calculate the root
//! in a more efficient way.
//!
//! When using a cache, be careful to properly invalidate cache entries whenever you perform
//! modifications on the trie associated to it.

use super::{
    nibble::{bytes_to_nibbles, Nibble},
    trie_node, trie_structure, TrieEntryVersion,
};

use core::{fmt, iter};

/// Cache containing intermediate calculation steps.
///
/// If the storage's content is modified, you **must** call the appropriate methods to invalidate
/// entries. Otherwise, the trie root calculation will yield an incorrect result.
#[derive(Clone)]
pub struct CalculationCache {
    /// Structure of the trie.
    /// If `Some`, the structure is either fully conforming to the trie.
    structure: Option<trie_structure::TrieStructure<CacheEntry>>,
}

/// Custom data stored in each node in [`CalculationCache::structure`].
#[derive(Default, Clone)]
struct CacheEntry {
    merkle_value: Option<trie_node::MerkleValueOutput>,
}

impl CalculationCache {
    /// Builds a new empty cache.
    pub const fn empty() -> Self {
        CalculationCache { structure: None }
    }

    /// Notify the cache that a storage value at the given key has been added, modified or removed.
    ///
    /// `has_value` must be true if there is now a storage value at the given key.
    pub fn storage_value_update(&mut self, key: &[u8], has_value: bool) {
        let structure = match &mut self.structure {
            Some(s) => s,
            None => return,
        };

        // Update the existing structure to account for the change.
        // The trie structure will report exactly how the trie is modified, which makes it
        // possible to know which nodes' Merkle values need to be invalidated.

        let mut node_to_invalidate = match (
            structure.node(bytes_to_nibbles(key.iter().copied())),
            has_value,
        ) {
            (trie_structure::Entry::Vacant(entry), true) => {
                match entry.insert_storage_value() {
                    trie_structure::PrepareInsert::One(insert) => {
                        let inserted = insert.insert(Default::default());
                        match inserted.into_parent() {
                            Some(p) => p,
                            None => return,
                        }
                    }
                    trie_structure::PrepareInsert::Two(insert) => {
                        let inserted = insert.insert(Default::default(), Default::default());

                        // We additionally have to invalidate the Merkle value of the children of
                        // the newly-inserted branch node.
                        let mut inserted_branch = inserted.into_parent().unwrap();
                        for idx in 0..16u8 {
                            if let Some(mut child) =
                                inserted_branch.child(Nibble::try_from(idx).unwrap())
                            {
                                child.user_data().merkle_value = None;
                            }
                        }

                        match inserted_branch.into_parent() {
                            Some(p) => p,
                            None => return,
                        }
                    }
                }
            }
            (trie_structure::Entry::Vacant(_), false) => return,
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(entry)), true) => {
                let entry = entry.insert_storage_value();
                trie_structure::NodeAccess::Storage(entry)
            }
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Storage(entry)), true) => {
                trie_structure::NodeAccess::Storage(entry)
            }
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(_)), false) => {
                return
            }
            (
                trie_structure::Entry::Occupied(trie_structure::NodeAccess::Storage(entry)),
                false,
            ) => match entry.remove() {
                trie_structure::Remove::StorageToBranch(node) => {
                    trie_structure::NodeAccess::Branch(node)
                }
                trie_structure::Remove::BranchAlsoRemoved { sibling, .. } => sibling,
                trie_structure::Remove::SingleRemoveChild { child, .. } => child,
                trie_structure::Remove::SingleRemoveNoChild { parent, .. } => parent,
                trie_structure::Remove::TrieNowEmpty { .. } => return,
            },
        };

        // We invalidate the Merkle value of `node_to_invalidate` and all its ancestors.
        node_to_invalidate.user_data().merkle_value = None;
        let mut parent = node_to_invalidate.into_parent();
        while let Some(mut node) = parent.take() {
            // If the node has already had its Merkle value invalidated, then
            // we can stop there.
            if node.user_data().merkle_value.is_none() {
                break;
            }
            node.user_data().merkle_value = None;
            parent = node.into_parent();
        }
    }

    /// Notify the cache that all the storage values whose key start with the given prefix have
    /// been removed.
    pub fn prefix_remove_update(&mut self, prefix: &[u8]) {
        let structure = match &mut self.structure {
            Some(s) => s,
            None => return,
        };

        if let Some(mut node) = structure.remove_prefix(bytes_to_nibbles(prefix.iter().cloned())) {
            node.user_data().merkle_value = None;
            let mut parent = node.into_parent();
            while let Some(mut p) = parent.take() {
                p.user_data().merkle_value = None;
                parent = p.into_parent();
            }
        } else if let Some(mut root_node) = structure.root_node() {
            root_node.user_data().merkle_value = None;
        }
    }
}

impl Default for CalculationCache {
    fn default() -> Self {
        Self::empty()
    }
}

impl fmt::Debug for CalculationCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The calculation cache is so large that printing its content is basically useless.
        f.debug_tuple("CalculationCache").finish()
    }
}

/// Start calculating the Merkle value of the root node.
pub fn root_merkle_value(cache: Option<CalculationCache>) -> RootMerkleValueCalculation {
    // The calculation that we perform relies on storing values in the cache and reloading them
    // afterwards. If the user didn't pass any cache, we create a temporary one.
    let cache_or_temporary = if let Some(mut cache) = cache {
        if let Some(structure) = &mut cache.structure {
            if structure.capacity() > structure.len().saturating_mul(2) {
                structure.shrink_to_fit();
            }
        }
        cache
    } else {
        CalculationCache::empty()
    };

    CalcInner {
        cache: cache_or_temporary,
        current: None,
        coming_from_child: false,
    }
    .next()
}

/// Current state of the [`RootMerkleValueCalculation`] and how to continue.
#[must_use]
pub enum RootMerkleValueCalculation {
    /// The calculation is finished.
    Finished {
        /// Root hash that has been calculated.
        hash: [u8; 32],
        /// Cache of the calculation that can be passed next time.
        cache: CalculationCache,
    },

    /// Request to return the list of all the keys in the trie. Call [`AllKeys::inject`] to
    /// indicate this list.
    AllKeys(AllKeys),

    /// Request the value of the node with a specific key. Call [`StorageValue::inject`] to
    /// indicate the value.
    StorageValue(StorageValue),
}

/// Calculation of the Merkle value is ready to continue.
/// Shared by all the public-facing structs.
///
/// # Implementation notes
///
/// We traverse the trie in attempt to find missing Merkle values.
/// We start with the root node. For each node, if its Merkle value is absent, we continue
/// iterating with its first child. If its Merkle value is present, we continue iterating with
/// the next sibling or, if it is the last sibling, the parent. In that situation where we jump
/// from last sibling to parent, we also calculate the parent's Merkle value in the process.
/// Due to this order of iteration, we traverse each node which lack a Merkle value twice, and
/// the Merkle value is calculated that second time.
struct CalcInner {
    /// Contains the intermediary steps of the calculation. `None` if the calculation is finished.
    cache: CalculationCache,

    /// Index within `cache` of the node currently being iterated.
    current: Option<trie_structure::NodeIndex>,

    // `coming_from_child` is used to differentiate whether the previous iteration was the
    // previous sibling of `current` or the last child of `current`.
    coming_from_child: bool,
}

impl CalcInner {
    /// Advances the calculation to the next step.
    fn next(mut self) -> RootMerkleValueCalculation {
        // Make sure that `cache.structure` contains a trie structure that matches the trie.
        if self.cache.structure.is_none() {
            return RootMerkleValueCalculation::AllKeys(AllKeys { calculation: self });
        }

        // At this point `trie_structure` is guaranteed to match the trie, but its Merkle values
        // might be missing and need to be filled.
        let trie_structure = self.cache.structure.as_mut().unwrap();

        // Node currently being iterated.
        let mut current: trie_structure::NodeAccess<_> = {
            if self.current.is_none() {
                self.current = match trie_structure.root_node() {
                    Some(c) => Some(c.node_index()),
                    None => {
                        // Trie is empty.
                        // `calculate_merkle_value` can only return an error if the partial key
                        // isn't empty, meaning that it is safe to unwrap.
                        let merkle_value = trie_node::calculate_merkle_value(
                            trie_node::Decoded {
                                partial_key: iter::empty(),
                                children: [None::<&'static [u8]>; 16],
                                storage_value: trie_node::StorageValue::None,
                            },
                            true,
                        )
                        .unwrap();

                        return RootMerkleValueCalculation::Finished {
                            hash: merkle_value.into(),
                            cache: self.cache,
                        };
                    }
                };
            }

            trie_structure.node_by_index(self.current.unwrap()).unwrap()
        };

        loop {
            // If we already have a Merkle value, jump either to the next sibling (if any), or back
            // to the parent.
            if current.user_data().merkle_value.is_some() {
                match current.into_next_sibling() {
                    Ok(sibling) => {
                        current = sibling;
                        self.current = Some(current.node_index());
                        self.coming_from_child = false;
                        continue;
                    }
                    Err(curr) => {
                        if let Some(parent) = curr.into_parent() {
                            current = parent;
                            self.current = Some(current.node_index());
                            self.coming_from_child = true;
                            continue;
                        }
                        // No next sibling nor parent. We have finished traversing the tree.
                        let mut root_node = trie_structure.root_node().unwrap();
                        let merkle_value = root_node.user_data().merkle_value.clone().unwrap();
                        return RootMerkleValueCalculation::Finished {
                            hash: merkle_value.into(),
                            cache: self.cache,
                        };
                    }
                }
            }

            debug_assert!(current.user_data().merkle_value.is_none());

            // If previous iteration is from `current`'s previous sibling, we jump down to
            // `current`'s children.
            if !self.coming_from_child {
                match current.into_first_child() {
                    Err(c) => current = c,
                    Ok(first_child) => {
                        current = first_child;
                        self.current = Some(current.node_index());
                        self.coming_from_child = false;
                        continue;
                    }
                }
            }

            // If we reach this, we are ready to calculate `current`'s Merkle value.
            self.coming_from_child = true;

            if !current.has_storage_value() {
                // Calculate the Merkle value of the node.
                // `calculate_merkle_value` returns an error if the node is invalid, which would
                // indicate a bug in this module.
                let merkle_value = trie_node::calculate_merkle_value(
                    trie_node::Decoded {
                        partial_key: current.partial_key(),
                        children: core::array::from_fn(|child_idx| {
                            current
                                .child_user_data(
                                    Nibble::try_from(u8::try_from(child_idx).unwrap()).unwrap(),
                                )
                                .map(|child| child.merkle_value.as_ref().unwrap())
                        }),
                        storage_value: trie_node::StorageValue::None,
                    },
                    current.is_root_node(),
                )
                .unwrap();

                current.user_data().merkle_value = Some(merkle_value);
                continue;
            }

            return RootMerkleValueCalculation::StorageValue(StorageValue { calculation: self });
        }
    }
}

/// Request to return the list of all the keys in the storage. Call [`AllKeys::inject`] to indicate
/// this list.
#[must_use]
pub struct AllKeys {
    calculation: CalcInner,
}

impl AllKeys {
    /// Indicates the list of all keys of the trie and advances the calculation.
    pub fn inject(
        mut self,
        keys: impl Iterator<Item = impl Iterator<Item = u8> + Clone>,
    ) -> RootMerkleValueCalculation {
        debug_assert!(self.calculation.cache.structure.is_none());
        self.calculation.cache.structure = Some({
            let mut structure = trie_structure::TrieStructure::new();
            for key in keys {
                structure
                    .node(bytes_to_nibbles(key))
                    .into_vacant()
                    .unwrap()
                    .insert_storage_value()
                    .insert(Default::default(), Default::default());
            }
            structure
        });
        self.calculation.next()
    }
}

/// Request the value of the node with a specific key. Call [`StorageValue::inject`] to indicate
/// the value.
#[must_use]
pub struct StorageValue {
    calculation: CalcInner,
}

impl StorageValue {
    /// Returns the key whose value is being requested.
    pub fn key(&'_ self) -> impl Iterator<Item = u8> + '_ {
        let trie_structure = self.calculation.cache.structure.as_ref().unwrap();
        let mut full_key = trie_structure
            .node_full_key_by_index(self.calculation.current.unwrap())
            .unwrap();
        iter::from_fn(move || {
            let nibble1 = full_key.next()?;
            let nibble2 = full_key.next().unwrap();
            let val = (u8::from(nibble1) << 4) | u8::from(nibble2);
            Some(val)
        })
    }

    /// Indicates the storage value and advances the calculation.
    pub fn inject(
        mut self,
        stored_value: Option<(impl AsRef<[u8]>, TrieEntryVersion)>,
    ) -> RootMerkleValueCalculation {
        let trie_structure = self.calculation.cache.structure.as_mut().unwrap();
        let mut current: trie_structure::NodeAccess<_> = trie_structure
            .node_by_index(self.calculation.current.unwrap())
            .unwrap();

        // Due to borrowing issues, we need to build the hash of the storage value ahead of time
        // if necessary.
        let hashed_storage_value = match &stored_value {
            Some((_, TrieEntryVersion::V0)) => None,
            Some((value, TrieEntryVersion::V1)) if value.as_ref().len() >= 33 => {
                Some(blake2_rfc::blake2b::blake2b(32, &[], value.as_ref()))
            }
            Some((_, TrieEntryVersion::V1)) => None,
            None => {
                // API user misbehaved.
                panic!("Injected no value when previously reported a value at this key")
            }
        };

        // Calculate the Merkle value of the node.
        // `calculate_merkle_value` can only return an error if the node is invalid, which would
        // indicate a serious bug in this module.
        let merkle_value = trie_node::calculate_merkle_value(
            trie_node::Decoded {
                partial_key: current.partial_key(),
                children: core::array::from_fn(|child_idx| {
                    current
                        .child_user_data(
                            Nibble::try_from(u8::try_from(child_idx).unwrap()).unwrap(),
                        )
                        .map(|child| child.merkle_value.as_ref().unwrap())
                }),
                storage_value: match &hashed_storage_value {
                    None => {
                        trie_node::StorageValue::Unhashed(stored_value.as_ref().unwrap().0.as_ref())
                    }
                    Some(hashed_storage_value) => trie_node::StorageValue::Hashed(
                        <&[u8; 32]>::try_from(hashed_storage_value.as_bytes()).unwrap(),
                    ),
                },
            },
            current.is_root_node(),
        )
        .unwrap();

        current.user_data().merkle_value = Some(merkle_value);
        self.calculation.next()
    }
}

#[cfg(test)]
mod tests {
    use crate::trie::TrieEntryVersion;
    use alloc::collections::BTreeMap;
    use rand::{seq::IteratorRandom as _, Rng as _};

    fn calculate_root(version: TrieEntryVersion, trie: &BTreeMap<Vec<u8>, Vec<u8>>) -> [u8; 32] {
        let mut calculation = super::root_merkle_value(None);

        loop {
            match calculation {
                super::RootMerkleValueCalculation::Finished { hash, .. } => {
                    return hash;
                }
                super::RootMerkleValueCalculation::AllKeys(keys) => {
                    calculation = keys.inject(trie.keys().map(|k| k.iter().cloned()));
                }
                super::RootMerkleValueCalculation::StorageValue(value) => {
                    let key = value.key().collect::<Vec<u8>>();
                    calculation = value.inject(trie.get(&key).map(|v| (v, version)));
                }
            }
        }
    }

    #[test]
    fn trie_root_one_node() {
        let mut trie = BTreeMap::new();
        trie.insert(b"abcd".to_vec(), b"hello world".to_vec());

        let expected = [
            122, 177, 134, 89, 211, 178, 120, 158, 242, 64, 13, 16, 113, 4, 199, 212, 251, 147,
            208, 109, 154, 182, 168, 182, 65, 165, 222, 124, 63, 236, 200, 81,
        ];

        assert_eq!(calculate_root(TrieEntryVersion::V0, &trie), &expected[..]);
        assert_eq!(calculate_root(TrieEntryVersion::V1, &trie), &expected[..]);
    }

    #[test]
    fn trie_root_empty() {
        let trie = BTreeMap::new();
        let expected = blake2_rfc::blake2b::blake2b(32, &[], &[0x0]);
        assert_eq!(
            calculate_root(TrieEntryVersion::V0, &trie),
            expected.as_bytes()
        );
        assert_eq!(
            calculate_root(TrieEntryVersion::V1, &trie),
            expected.as_bytes()
        );
    }

    #[test]
    fn trie_root_single_tuple() {
        let mut trie = BTreeMap::new();
        trie.insert([0xaa].to_vec(), [0xbb].to_vec());

        let expected = blake2_rfc::blake2b::blake2b(
            32,
            &[],
            &[
                0x42,   // leaf 0x40 (2^6) with (+) key of 2 nibbles (0x02)
                0xaa,   // key data
                1 << 2, // length of value in bytes as Compact
                0xbb,   // value data
            ],
        );

        assert_eq!(
            calculate_root(TrieEntryVersion::V0, &trie),
            expected.as_bytes()
        );
        assert_eq!(
            calculate_root(TrieEntryVersion::V1, &trie),
            expected.as_bytes()
        );
    }

    #[test]
    fn trie_root_example() {
        let mut trie = BTreeMap::new();
        trie.insert([0x48, 0x19].to_vec(), [0xfe].to_vec());
        trie.insert([0x13, 0x14].to_vec(), [0xff].to_vec());

        let ex = vec![
            0x80,      // branch, no value (0b_10..) no nibble
            0x12,      // slots 1 & 4 are taken from 0-7
            0x00,      // no slots from 8-15
            0x05 << 2, // first slot: LEAF, 5 bytes long.
            0x43,      // leaf 0x40 with 3 nibbles
            0x03,      // first nibble
            0x14,      // second & third nibble
            0x01 << 2, // 1 byte data
            0xff,      // value data
            0x05 << 2, // second slot: LEAF, 5 bytes long.
            0x43,      // leaf with 3 nibbles
            0x08,      // first nibble
            0x19,      // second & third nibble
            0x01 << 2, // 1 byte data
            0xfe,      // value data
        ];

        let expected = blake2_rfc::blake2b::blake2b(32, &[], &ex);
        assert_eq!(
            calculate_root(TrieEntryVersion::V0, &trie),
            expected.as_bytes()
        );
        assert_eq!(
            calculate_root(TrieEntryVersion::V1, &trie),
            expected.as_bytes()
        );
    }

    #[test]
    fn cache_up_to_date() {
        // This test builds a random trie, then calculates its root, then randomly modifies that
        // trie, then calculates its root again twice, with and without the cache used in the
        // first computation, and compares the two results.

        // Run the test many times, as it relies on randomness.
        for _ in 0..1000 {
            // Generate a random trie.
            let mut trie = {
                let mut trie = BTreeMap::<Vec<u8>, Vec<u8>>::new();

                for _ in 0..rand::thread_rng().gen_range::<u32, _>(5..400) {
                    let mut new_key = trie
                        .keys()
                        .choose(&mut rand::thread_rng())
                        .map(|s| s.to_vec())
                        .unwrap_or(Vec::new());
                    for _ in 0..rand::thread_rng().gen_range::<u32, _>(1..6) {
                        new_key.push(rand::random::<u8>());
                    }
                    let mut new_value = vec![0u8; 50];
                    rand::thread_rng().fill(&mut new_value[..]);
                    trie.insert(new_key, new_value);
                }

                trie
            };

            // Calculate its root.
            // We don't actually care about the root hash. We just want the cache.
            let mut cache = {
                let mut calculation = super::root_merkle_value(None);
                loop {
                    match calculation {
                        super::RootMerkleValueCalculation::Finished { cache, .. } => {
                            break cache;
                        }
                        super::RootMerkleValueCalculation::AllKeys(keys) => {
                            calculation = keys.inject(trie.keys().map(|k| k.iter().cloned()));
                        }
                        super::RootMerkleValueCalculation::StorageValue(value) => {
                            let key = value.key().collect::<Vec<u8>>();
                            calculation =
                                value.inject(trie.get(&key).map(|v| (v, TrieEntryVersion::V1)));
                        }
                    }
                }
            };

            // Now modify the first trie, flushing the corresponding cache entries.
            // We perform very few modifications. Cache flushes removes information from the
            // cache. The more modifications the more information is removed, and thus the higher
            // the chances that we don't detect a bug causing obsolete information to remain in
            // the cache.
            // TODO: this test doesn't clear prefixes, even though it should, because `prefix_remove_update` is implemented in a dummy way that would make the test pointless
            for _ in 0..rand::thread_rng().gen_range::<u32, _>(1..5) {
                let key_to_tweak = match trie.keys().choose(&mut rand::thread_rng()) {
                    Some(k) => k.to_vec(),
                    None => break,
                };

                if rand::random() {
                    // Modify the key.
                    cache.storage_value_update(&key_to_tweak, true);
                    let mut new_value = vec![0u8; 50];
                    rand::thread_rng().fill(&mut new_value[..]);
                    trie.insert(key_to_tweak, new_value);
                } else {
                    // Remove the key.
                    cache.storage_value_update(&key_to_tweak, false);
                    trie.remove(&key_to_tweak);
                }
            }

            // Now calculate the root again, without a cache.
            let root_no_cache = calculate_root(TrieEntryVersion::V1, &trie);

            // Now calculate the root again, with a cache.
            let root_with_cache = {
                let mut calculation = super::root_merkle_value(Some(cache));
                loop {
                    match calculation {
                        super::RootMerkleValueCalculation::Finished { hash, .. } => {
                            break hash;
                        }
                        super::RootMerkleValueCalculation::AllKeys(keys) => {
                            calculation = keys.inject(trie.keys().map(|k| k.iter().cloned()));
                        }
                        super::RootMerkleValueCalculation::StorageValue(value) => {
                            let key = value.key().collect::<Vec<u8>>();
                            calculation =
                                value.inject(trie.get(&key).map(|v| (v, TrieEntryVersion::V1)));
                        }
                    }
                }
            };

            // Make sure they're equal.
            assert_eq!(root_no_cache, root_with_cache);
        }
    }
}
