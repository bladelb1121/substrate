// This file is part of Substrate.

// Copyright (C) 2017-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The overlayed changes to state.

use crate::{
	backend::Backend, ChangesTrieTransaction,
	changes_trie::{
		NO_EXTRINSIC_INDEX, BlockNumber, build_changes_trie,
		State as ChangesTrieState,
	},
	stats::StateMachineStats,
};

use std::{ops, collections::{HashMap, BTreeMap, BTreeSet, HashSet}};
use codec::{Decode, Encode};
use sp_core::storage::{well_known_keys::EXTRINSIC_INDEX, ChildInfo, ChildType};
use sp_core::offchain::storage::OffchainOverlayedChanges;
use itertools::Itertools;

use hash_db::Hasher;

/// Storage key.
pub type StorageKey = Vec<u8>;

/// Storage value.
pub type StorageValue = Vec<u8>;

/// In memory array of storage values.
pub type StorageCollection = Vec<(StorageKey, Option<StorageValue>)>;

/// In memory arrays of storage values for multiple child tries.
pub type ChildStorageCollection = Vec<(StorageKey, StorageCollection)>;

/// The overlayed changes to state to be queried on top of the backend.
///
/// A transaction shares all prospective changes within an inner overlay
/// that can be cleared.
#[derive(Debug, Default, Clone)]
pub struct OverlayedChanges {
	/// Top level storage changes.
	top: OverlayedChangeSet,
	/// Child storage changes. The map key is the child storage key without the common prefix.
	children: HashMap<StorageKey, (OverlayedChangeSet, ChildInfo)>, 
	/// True if extrinsics stats must be collected.
	collect_extrinsics: bool,
	/// Collect statistic on this execution.
	stats: StateMachineStats,
}

/// The storage value, used inside OverlayedChanges.
#[derive(Debug, Default, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct OverlayedValue {
	transactions: Vec<InnerValue>,
}

#[derive(Debug, Default, Clone)]
#[cfg_attr(test, derive(PartialEq))]
struct InnerValue {
	/// Current value. None if value has been deleted. One value per open nested transaction.
	value: Option<StorageValue>,
	/// The set of extrinsic indices where the values has been changed.
	/// Is filled only if runtime has announced changes trie support.
	extrinsics: BTreeSet<u32>,
}

#[derive(Debug, Default, Clone)]
struct OverlayedChangeSet {
	/// Stores the actual changes.
	changes: BTreeMap<StorageKey, OverlayedValue>,
	/// Stores which keys are dirty per transaction. Needed in order to determine which
	/// values to merge into the parent transaction on commit.
	dirty_keys: Vec<HashSet<StorageKey>>,
}

/// A storage changes structure that can be generated by the data collected in [`OverlayedChanges`].
///
/// This contains all the changes to the storage and transactions to apply theses changes to the
/// backend.
pub struct StorageChanges<Transaction, H: Hasher, N: BlockNumber> {
	/// All changes to the main storage.
	///
	/// A value of `None` means that it was deleted.
	pub main_storage_changes: StorageCollection,
	/// All changes to the child storages.
	pub child_storage_changes: ChildStorageCollection,
	/// Offchain state changes to write to the offchain database.
	pub offchain_storage_changes: OffchainOverlayedChanges,
	/// A transaction for the backend that contains all changes from
	/// [`main_storage_changes`](StorageChanges::main_storage_changes) and from
	/// [`child_storage_changes`](StorageChanges::child_storage_changes).
	/// [`offchain_storage_changes`](StorageChanges::offchain_storage_changes).
	pub transaction: Transaction,
	/// The storage root after applying the transaction.
	pub transaction_storage_root: H::Out,
	/// Contains the transaction for the backend for the changes trie.
	///
	/// If changes trie is disabled the value is set to `None`.
	pub changes_trie_transaction: Option<ChangesTrieTransaction<H, N>>,
}

impl<Transaction, H: Hasher, N: BlockNumber> StorageChanges<Transaction, H, N> {
	/// Deconstruct into the inner values
	pub fn into_inner(self) -> (
		StorageCollection,
		ChildStorageCollection,
		OffchainOverlayedChanges,
		Transaction,
		H::Out,
		Option<ChangesTrieTransaction<H, N>>,
	) {
		(
			self.main_storage_changes,
			self.child_storage_changes,
			self.offchain_storage_changes,
			self.transaction,
			self.transaction_storage_root,
			self.changes_trie_transaction,
		)
	}
}

/// The storage transaction are calculated as part of the `storage_root` and
/// `changes_trie_storage_root`. These transactions can be reused for importing the block into the
/// storage. So, we cache them to not require a recomputation of those transactions.
pub struct StorageTransactionCache<Transaction, H: Hasher, N: BlockNumber> {
	/// Contains the changes for the main and the child storages as one transaction.
	pub(crate) transaction: Option<Transaction>,
	/// The storage root after applying the transaction.
	pub(crate) transaction_storage_root: Option<H::Out>,
	/// Contains the changes trie transaction.
	pub(crate) changes_trie_transaction: Option<Option<ChangesTrieTransaction<H, N>>>,
	/// The storage root after applying the changes trie transaction.
	pub(crate) changes_trie_transaction_storage_root: Option<Option<H::Out>>,
}

impl<Transaction, H: Hasher, N: BlockNumber> StorageTransactionCache<Transaction, H, N> {
	/// Reset the cached transactions.
	pub fn reset(&mut self) {
		*self = Self::default();
	}
}

impl<Transaction, H: Hasher, N: BlockNumber> Default for StorageTransactionCache<Transaction, H, N> {
	fn default() -> Self {
		Self {
			transaction: None,
			transaction_storage_root: None,
			changes_trie_transaction: None,
			changes_trie_transaction_storage_root: None,
		}
	}
}

impl<Transaction: Default, H: Hasher, N: BlockNumber> Default for StorageChanges<Transaction, H, N> {
	fn default() -> Self {
		Self {
			main_storage_changes: Default::default(),
			child_storage_changes: Default::default(),
			offchain_storage_changes: Default::default(),
			transaction: Default::default(),
			transaction_storage_root: Default::default(),
			changes_trie_transaction: None,
		}
	}
}

#[cfg(test)]
impl std::iter::FromIterator<(StorageKey, OverlayedValue)> for OverlayedChangeSet {
	fn from_iter<T: IntoIterator<Item = (StorageKey, OverlayedValue)>>(iter: T) -> Self {
		Self {
			changes: iter.into_iter().collect(),
			.. Default::default()
		}
	}
}

impl OverlayedValue {
	/// The most recent value contained in this overlay.
	pub fn value(&self) -> Option<&StorageValue> {
		self.transactions.last()
			.expect("A StorageValue is always initialized with one value.\
			The last element is never removed as those are committed changes.")
			.value
			.as_ref()
	}

	/// List of indices of extrinsics which modified the value using this overlay.
	pub fn extrinsics(&self) -> impl Iterator<Item=&u32> {
		self.transactions.iter().flat_map(|t| t.extrinsics.iter()).unique()
	}

	fn value_mut(&mut self) -> &mut Option<StorageValue> {
		&mut self.transactions.last_mut()
			.expect("A StorageValue is always initialized with one value.\
			The last element is never removed as those are committed changes.")
			.value
	}

	fn tx_extrinsics_mut(&mut self) -> &mut BTreeSet<u32> {
		&mut self.transactions.last_mut().expect("").extrinsics
	}
}

impl OverlayedChangeSet {
	fn is_empty(&self) -> bool {
		self.changes.is_empty()
	}

	fn get(&self, key: &[u8]) -> Option<&OverlayedValue> {
		self.changes.get(key)
	}

	#[must_use = "A change was registered, so this value MUST be modified."]
	fn modify(
		&mut self,
		key: &[u8],
		at_extrinsic: Option<u32>,
		init: impl FnOnce() -> StorageValue
	) -> &mut OverlayedValue {
		let first_write_in_tx = if let Some(dirty_keys) = self.dirty_keys.last_mut() {
			dirty_keys.insert(key.to_vec())
		} else {
			false
		};

		let value = self.changes.entry(key.to_vec()).or_insert_with(Default::default);

		if first_write_in_tx || value.transactions.is_empty() {
			if let Some(val) = value.transactions.last().map(|val| val.value.clone()) {
				value.transactions.push(InnerValue {
					value: val,
					.. Default::default()
				})
			} else {
				value.transactions.push(InnerValue {
					value: Some(init()),
					.. Default::default()
				});
			}
		}

		if let Some(extrinsic) = at_extrinsic {
			value.tx_extrinsics_mut().insert(extrinsic);
		}

		value
	}

	fn set(&mut self, key: &[u8], val: Option<StorageValue>, at_extrinsic: Option<u32>) {
		let first_write_in_tx = if let Some(dirty_keys) = self.dirty_keys.last_mut() {
			dirty_keys.insert(key.to_vec())
		} else {
			false
		};

		let value = self.changes.entry(key.to_vec()).or_insert_with(Default::default);

		if first_write_in_tx || value.transactions.is_empty() {
			value.transactions.push(InnerValue {
				value: val,
				.. Default::default()
			});
		} else {
			*value.value_mut() = val;
		}

		if let Some(extrinsic) = at_extrinsic {
			value.tx_extrinsics_mut().insert(extrinsic);
		}
	}

	fn start_transaction(&mut self) {
		self.dirty_keys.push(Default::default());
	}

	fn rollback_transaction(&mut self) {
		for key in self.dirty_keys.pop().expect("Transactions must be balanced.") {
			let value = self.changes.get_mut(&key).expect("Key was marked as dirty.");
			value.transactions.pop();

			// We just rolled backed the last transaction and no value is in the
			// committed set. We need to remove the key as an `OverlayValue` with no
			// contents at all violates its invariant of always having at least one value.
			if self.dirty_keys.is_empty() && value.transactions.is_empty() {
				self.changes.remove(&key);
			}
		}
	}

	fn commit_transaction(&mut self) {
		for key in self.dirty_keys.pop().expect("Transactions must be balanced.") {
			let value = self.changes.get_mut(&key).expect("Key was marked as dirty.");
			let merge_tx = ! if let Some(dirty_keys) = self.dirty_keys.last_mut() {
				// Not the last tx: Did the previous tx write to this key?
				dirty_keys.insert(key)
			} else {
				// Last tx: Is there already a value in the committed set?
				// Check against one rather than empty because the current tx is still
				// in the list as it is popped later in this function.
				value.transactions.len() == 1
			};

			// No need to merge if the previous tx has never written to this key.
			// We just use the current tx as the previous one.
			if ! merge_tx {
				return;
			}


			let dropped_tx = value.transactions.pop().expect("Key was marked dirty for this tx");
			*value.value_mut() = dropped_tx.value;
			value.tx_extrinsics_mut().extend(dropped_tx.extrinsics);
		}
	}
}

impl OverlayedChanges {
	/// Whether the overlayed changes are empty.
	pub fn is_empty(&self) -> bool {
		self.top.is_empty() && self.children.is_empty()
	}

	/// Ask to collect/not to collect extrinsics indices where key(s) has been changed.
	pub fn set_collect_extrinsics(&mut self, collect_extrinsics: bool) {
		self.collect_extrinsics = collect_extrinsics;
	}

	/// Returns a double-Option: None if the key is unknown (i.e. and the query should be referred
	/// to the backend); Some(None) if the key has been deleted. Some(Some(...)) for a key whose
	/// value has been set.
	pub fn storage(&self, key: &[u8]) -> Option<Option<&[u8]>> {
		self.top.get(key).map(|x| {
			let value = x.value();
			let size_read = value.map(|x| x.len() as u64).unwrap_or(0);
			self.stats.tally_read_modified(size_read);
			value.map(AsRef::as_ref)
		})
	}

	/// Returns mutable reference to current changed value (prospective).
	/// If there is no value in the overlay, the default callback is used to initiate
	/// the value.
	/// Warning this function register a change, so the mutable reference MUST be modified.
	#[must_use = "A change was registered, so this value MUST be modified."]
	pub fn value_mut_or_insert_with(
		&mut self,
		key: &[u8],
		init: impl Fn() -> StorageValue,
	) -> &mut StorageValue {
		let entry = self.top.modify(key, self.extrinsic_index(), init);
		let value = entry.value_mut();

		//if was deleted initialise back with empty vec
		if value.is_none() {
			*value = Some(Default::default());
		}

		value.as_mut().expect("Initialized above; qed")
	}

	/// Returns a double-Option: None if the key is unknown (i.e. and the query should be referred
	/// to the backend); Some(None) if the key has been deleted. Some(Some(...)) for a key whose
	/// value has been set.
	pub fn child_storage(&self, child_info: &ChildInfo, key: &[u8]) -> Option<Option<&[u8]>> {
		if let Some(map) = self.children.get(child_info.storage_key()) {
			if let Some(val) = map.0.get(key) {
				let value = val.value();
				let size_read = value.map(|x| x.len() as u64).unwrap_or(0);
				self.stats.tally_read_modified(size_read);
				return Some(value.map(AsRef::as_ref));
			}
		}
		None
	}

	/// Inserts the given key-value pair into the prospective change set.
	///
	/// `None` can be used to delete a value specified by the given key.
	pub(crate) fn set_storage(&mut self, key: StorageKey, val: Option<StorageValue>) {
		let size_write = val.as_ref().map(|x| x.len() as u64).unwrap_or(0);
		self.stats.tally_write_overlay(size_write);
		self.top.set(&key, val, self.extrinsic_index());
	}

	/// Inserts the given key-value pair into the prospective child change set.
	///
	/// `None` can be used to delete a value specified by the given key.
	pub(crate) fn set_child_storage(
		&mut self,
		child_info: &ChildInfo,
		key: StorageKey,
		val: Option<StorageValue>,
	) {
		let extrinsic_index = self.extrinsic_index();
		let size_write = val.as_ref().map(|x| x.len() as u64).unwrap_or(0);
		self.stats.tally_write_overlay(size_write);
		let storage_key = child_info.storage_key().to_vec();
		let map_entry = self.children.entry(storage_key)
			.or_insert_with(|| (Default::default(), child_info.to_owned()));
		let updatable = map_entry.1.try_update(child_info);
		debug_assert!(updatable);

		map_entry.0.set(&key, val, extrinsic_index);
	}

	/// Clear child storage of given storage key.
	///
	/// NOTE that this doesn't take place immediately but written into the prospective
	/// change set, and still can be reverted by [`discard_prospective`].
	///
	/// [`discard_prospective`]: #method.discard_prospective
	pub(crate) fn clear_child_storage(
		&mut self,
		child_info: &ChildInfo,
	) {
		let extrinsic_index = self.extrinsic_index();
		let storage_key = child_info.storage_key();
		let (changeset, info) = self.children.entry(storage_key.to_vec())
			.or_insert_with(|| (Default::default(), child_info.to_owned()));
		let updatable = info.try_update(child_info);
		debug_assert!(updatable);

		for (key, _) in changeset.changes {
			changeset.set(&key, None, extrinsic_index)
		}
	}

	/// Removes all key-value pairs which keys share the given prefix.
	///
	/// NOTE that this doesn't take place immediately but written into the prospective
	/// change set, and still can be reverted by [`discard_prospective`].
	///
	/// [`discard_prospective`]: #method.discard_prospective
	pub(crate) fn clear_prefix(&mut self, prefix: &[u8]) {
		for (key, _) in self.top.changes.iter().filter(|(key, _)| key.starts_with(prefix)) {
			self.top.set(key, None, self.extrinsic_index())
		}
	}

	pub(crate) fn clear_child_prefix(
		&mut self,
		child_info: &ChildInfo,
		prefix: &[u8],
	) {
		let extrinsic_index = self.extrinsic_index();
		let storage_key = child_info.storage_key();
		let (changeset, info) = self.children.entry(storage_key.to_vec())
			.or_insert_with(|| (Default::default(), child_info.to_owned()));
		let updatable = info.try_update(child_info);
		debug_assert!(updatable);

		for (key, _) in changeset.changes.iter().filter(|(key, _)| key.starts_with(prefix)) {
			changeset.set(key, None, extrinsic_index);
		}
	}

	pub fn start_transaction(&mut self) {
		self.top.start_transaction();
		for (_, (changeset, _)) in self.children {
			changeset.start_transaction();
		}
	}

	pub fn rollback_transaction(&mut self) {
		self.top.rollback_transaction();
		for (_, (changeset, _)) in self.children {
			changeset.rollback_transaction();
		}
	}

	pub fn commit_transaction(&mut self) {
		self.top.commit_transaction();
		for (_, (changeset, _)) in self.children {
			changeset.commit_transaction();
		}
	}

	/// Consume `OverlayedChanges` and take committed set.
	///
	/// Panics:
	/// Will panic if there are any uncommitted prospective changes.
	fn drain_committed(&mut self) -> (
		impl Iterator<Item=(StorageKey, Option<StorageValue>)>,
		impl Iterator<Item=(StorageKey, (impl Iterator<Item=(StorageKey, Option<StorageValue>)>, ChildInfo))>,
	) {
		assert!(self.top.dirty_keys.is_empty());
		(
			std::mem::take(&mut self.top)
				.changes
				.into_iter()
				.map(|(k, mut v)|
					(k, v.transactions.pop().expect("Always at least one value").value)
				),
			std::mem::take(&mut self.children)
				.into_iter()
				.map(|(key, (val, info))| {
					assert!(val.dirty_keys.is_empty());
					(
						key,
						(val.changes.into_iter().map(|(k, mut v)|
							(k, v.transactions.pop().expect("Always at least one value.").value)), info
						)
					)
				}),
		)
	}

	/// Get an iterator over all pending and committed child tries in the overlay.
	pub fn child_infos(&self) -> impl Iterator<Item=&ChildInfo> {
		self.children.iter().map(|(_, v)| &v.1)
	}

	/// Get an iterator over all pending and committed changes.
	///
	/// Supplying `None` for `child_info` will only return changes that are in the top
	/// trie. Specifying some `child_info` will return only the changes in that
	/// child trie.
	pub fn changes(&self, child_info: Option<&ChildInfo>)
		-> impl Iterator<Item=(&StorageKey, &OverlayedValue)>
	{
		let overlay = if let Some(child_info) = child_info {
			match child_info.child_type() {
				ChildType::ParentKeyId =>
					self.children.get(child_info.storage_key()).map(|c| &c.0),
			}
		} else {
			Some(&self.top)
		};

		overlay.into_iter().flat_map(|overlay| &overlay.changes)
	}

	/// Convert this instance with all changes into a [`StorageChanges`] instance.
	pub fn into_storage_changes<
		B: Backend<H>, H: Hasher, N: BlockNumber
	>(
		mut self,
		backend: &B,
		changes_trie_state: Option<&ChangesTrieState<H, N>>,
		parent_hash: H::Out,
		mut cache: StorageTransactionCache<B::Transaction, H, N>,
	) -> Result<StorageChanges<B::Transaction, H, N>, String> where H::Out: Ord + Encode + 'static {
		self.drain_storage_changes(backend, changes_trie_state, parent_hash, &mut cache)
	}

	/// Drain all changes into a [`StorageChanges`] instance. Leave empty overlay in place.
	pub fn drain_storage_changes<B: Backend<H>, H: Hasher, N: BlockNumber>(
		&mut self,
		backend: &B,
		changes_trie_state: Option<&ChangesTrieState<H, N>>,
		parent_hash: H::Out,
		mut cache: &mut StorageTransactionCache<B::Transaction, H, N>,
	) -> Result<StorageChanges<B::Transaction, H, N>, String> where H::Out: Ord + Encode + 'static {
		// If the transaction does not exist, we generate it.
		if cache.transaction.is_none() {
			self.storage_root(backend, &mut cache);
		}

		let (transaction, transaction_storage_root) = cache.transaction.take()
			.and_then(|t| cache.transaction_storage_root.take().map(|tr| (t, tr)))
			.expect("Transaction was be generated as part of `storage_root`; qed");

		// If the transaction does not exist, we generate it.
		if cache.changes_trie_transaction.is_none() {
			self.changes_trie_root(
				backend,
				changes_trie_state,
				parent_hash,
				false,
				&mut cache,
			).map_err(|_| "Failed to generate changes trie transaction")?;
		}

		let changes_trie_transaction = cache.changes_trie_transaction
			.take()
			.expect("Changes trie transaction was generated by `changes_trie_root`; qed");

		let offchain_storage_changes = Default::default();
		let (main_storage_changes, child_storage_changes) = self.drain_committed();

		Ok(StorageChanges {
			main_storage_changes: main_storage_changes.collect(),
			child_storage_changes: child_storage_changes.map(|(sk, it)| (sk, it.0.collect())).collect(),
			offchain_storage_changes,
			transaction,
			transaction_storage_root,
			changes_trie_transaction,
		})
	}

	/// Inserts storage entry responsible for current extrinsic index.
	#[cfg(test)]
	pub(crate) fn set_extrinsic_index(&mut self, extrinsic_index: u32) {
		let val = self.top.modify(EXTRINSIC_INDEX.to_vec(), None);
		*val.value_mut() =  Some(extrinsic_index.encode());
		*val.tx_extrinsics_mut() = Default::default();
	}

	/// Returns current extrinsic index to use in changes trie construction.
	/// None is returned if it is not set or changes trie config is not set.
	/// Persistent value (from the backend) can be ignored because runtime must
	/// set this index before first and unset after last extrinsic is executed.
	/// Changes that are made outside of extrinsics, are marked with
	/// `NO_EXTRINSIC_INDEX` index.
	fn extrinsic_index(&self) -> Option<u32> {
		match self.collect_extrinsics {
			true => Some(
				self.storage(EXTRINSIC_INDEX)
					.and_then(|idx| idx.and_then(|idx| Decode::decode(&mut &*idx).ok()))
					.unwrap_or(NO_EXTRINSIC_INDEX)),
			false => None,
		}
	}

	/// Generate the storage root using `backend` and all changes from `prospective` and `committed`.
	///
	/// Returns the storage root and caches storage transaction in the given `cache`.
	pub fn storage_root<H: Hasher, N: BlockNumber, B: Backend<H>>(
		&self,
		backend: &B,
		cache: &mut StorageTransactionCache<B::Transaction, H, N>,
	) -> H::Out
		where H::Out: Ord + Encode,
	{
		let delta = self.changes(None).map(|(k, v)| (&k[..], v.value().map(|v| &v[..])));
		let child_delta = self.child_infos()
			.map(|info| (info, self.changes(Some(info)).map(
				|(k, v)| (&k[..], v.value().map(|v| &v[..]))
			)));

		let (root, transaction) = backend.full_storage_root(delta, child_delta);

		cache.transaction = Some(transaction);
		cache.transaction_storage_root = Some(root);

		root
	}

	/// Generate the changes trie root.
	///
	/// Returns the changes trie root and caches the storage transaction into the given `cache`.
	///
	/// # Panics
	///
	/// Panics on storage error, when `panic_on_storage_error` is set.
	pub fn changes_trie_root<'a, H: Hasher, N: BlockNumber, B: Backend<H>>(
		&self,
		backend: &B,
		changes_trie_state: Option<&'a ChangesTrieState<'a, H, N>>,
		parent_hash: H::Out,
		panic_on_storage_error: bool,
		cache: &mut StorageTransactionCache<B::Transaction, H, N>,
	) -> Result<Option<H::Out>, ()> where H::Out: Ord + Encode + 'static {
		build_changes_trie::<_, H, N>(
			backend,
			changes_trie_state,
			self,
			parent_hash,
			panic_on_storage_error,
		).map(|r| {
			let root = r.as_ref().map(|r| r.1).clone();
			cache.changes_trie_transaction = Some(r.map(|(db, _, cache)| (db, cache)));
			cache.changes_trie_transaction_storage_root = Some(root);
			root
		})
	}

	/// Get child info for a storage key.
	pub fn default_child_info(&self, storage_key: &[u8]) -> Option<&ChildInfo> {
		self.children.get(storage_key).map(|x| &x.1)
	}

	/// Returns the next (in lexicographic order) storage key in the overlayed alongside its value.
	/// If no value is next then `None` is returned.
	pub fn next_storage_key_change(&self, key: &[u8]) -> Option<(&[u8], &OverlayedValue)> {
		let range = (ops::Bound::Excluded(key), ops::Bound::Unbounded);
		self.top.changes.range::<[u8], _>(range).next().map(|(k, v)| (&k[..], v))
	}

	/// Returns the next (in lexicographic order) child storage key in the overlayed alongside its
	/// value.  If no value is next then `None` is returned.
	pub fn next_child_storage_key_change(
		&self,
		storage_key: &[u8],
		key: &[u8]
	) -> Option<(&[u8], &OverlayedValue)> {
		let range = (ops::Bound::Excluded(key), ops::Bound::Unbounded);

		self.children
			.get(storage_key)
			.and_then(|(overlay, _)|
				overlay.changes.range::<[u8], _>(range).next().map(|(k, v)| (&k[..], v))
			)
	}
}

#[cfg(test)]
impl From<Option<StorageValue>> for OverlayedValue {
	fn from(value: Option<StorageValue>) -> OverlayedValue {
		OverlayedValue { value, ..Default::default() }
	}
}

#[cfg(test)]
mod tests {
	use hex_literal::hex;
	use sp_core::{
		Blake2Hasher, traits::Externalities, storage::well_known_keys::EXTRINSIC_INDEX,
	};
	use crate::InMemoryBackend;
	use crate::ext::Ext;
	use super::*;

	fn strip_extrinsic_index(map: &BTreeMap<StorageKey, OverlayedValue>)
		-> BTreeMap<StorageKey, OverlayedValue>
	{
		let mut clone = map.clone();
		clone.remove(&EXTRINSIC_INDEX.to_vec());
		clone
	}

	#[test]
	fn overlayed_storage_works() {
		let mut overlayed = OverlayedChanges::default();

		let key = vec![42, 69, 169, 142];

		assert!(overlayed.storage(&key).is_none());

		overlayed.set_storage(key.clone(), Some(vec![1, 2, 3]));
		assert_eq!(overlayed.storage(&key).unwrap(), Some(&[1, 2, 3][..]));

		overlayed.commit_prospective();
		assert_eq!(overlayed.storage(&key).unwrap(), Some(&[1, 2, 3][..]));

		overlayed.set_storage(key.clone(), Some(vec![]));
		assert_eq!(overlayed.storage(&key).unwrap(), Some(&[][..]));

		overlayed.set_storage(key.clone(), None);
		assert!(overlayed.storage(&key).unwrap().is_none());

		overlayed.discard_prospective();
		assert_eq!(overlayed.storage(&key).unwrap(), Some(&[1, 2, 3][..]));

		overlayed.set_storage(key.clone(), None);
		overlayed.commit_prospective();
		assert!(overlayed.storage(&key).unwrap().is_none());
	}

	#[test]
	fn overlayed_storage_root_works() {
		let initial: BTreeMap<_, _> = vec![
			(b"doe".to_vec(), b"reindeer".to_vec()),
			(b"dog".to_vec(), b"puppyXXX".to_vec()),
			(b"dogglesworth".to_vec(), b"catXXX".to_vec()),
			(b"doug".to_vec(), b"notadog".to_vec()),
		].into_iter().collect();
		let backend = InMemoryBackend::<Blake2Hasher>::from(initial);
		let mut overlay = OverlayedChanges {
			committed: vec![
				(b"dog".to_vec(), Some(b"puppy".to_vec()).into()),
				(b"dogglesworth".to_vec(), Some(b"catYYY".to_vec()).into()),
				(b"doug".to_vec(), Some(vec![]).into()),
			].into_iter().collect(),
			prospective: vec![
				(b"dogglesworth".to_vec(), Some(b"cat".to_vec()).into()),
				(b"doug".to_vec(), None.into()),
			].into_iter().collect(),
			..Default::default()
		};

		let mut offchain_overlay = Default::default();
		let mut cache = StorageTransactionCache::default();
		let mut ext = Ext::new(
			&mut overlay,
			&mut offchain_overlay,
			&mut cache,
			&backend,
			crate::changes_trie::disabled_state::<_, u64>(),
			None,
		);
		const ROOT: [u8; 32] = hex!("39245109cef3758c2eed2ccba8d9b370a917850af3824bc8348d505df2c298fa");

		assert_eq!(&ext.storage_root()[..], &ROOT);
	}

	#[test]
	fn extrinsic_changes_are_collected() {
		let mut overlay = OverlayedChanges::default();
		overlay.set_collect_extrinsics(true);

		overlay.set_storage(vec![100], Some(vec![101]));

		overlay.set_extrinsic_index(0);
		overlay.set_storage(vec![1], Some(vec![2]));

		overlay.set_extrinsic_index(1);
		overlay.set_storage(vec![3], Some(vec![4]));

		overlay.set_extrinsic_index(2);
		overlay.set_storage(vec![1], Some(vec![6]));

		assert_eq!(strip_extrinsic_index(&overlay.prospective.top),
			vec![
				(vec![1], OverlayedValue { value: Some(vec![6]),
				 extrinsics: vec![0, 2].into_iter().collect() }),
				(vec![3], OverlayedValue { value: Some(vec![4]),
				 extrinsics: vec![1].into_iter().collect() }),
				(vec![100], OverlayedValue { value: Some(vec![101]),
				 extrinsics: vec![NO_EXTRINSIC_INDEX].into_iter().collect() }),
			].into_iter().collect());

		overlay.commit_prospective();

		overlay.set_extrinsic_index(3);
		overlay.set_storage(vec![3], Some(vec![7]));

		overlay.set_extrinsic_index(4);
		overlay.set_storage(vec![1], Some(vec![8]));

		assert_eq!(strip_extrinsic_index(&overlay.committed.top),
			vec![
				(vec![1], OverlayedValue { value: Some(vec![6]),
				 extrinsics: vec![0, 2].into_iter().collect() }),
				(vec![3], OverlayedValue { value: Some(vec![4]),
				 extrinsics: vec![1].into_iter().collect() }),
				(vec![100], OverlayedValue { value: Some(vec![101]),
				 extrinsics: vec![NO_EXTRINSIC_INDEX].into_iter().collect() }),
			].into_iter().collect());

		assert_eq!(strip_extrinsic_index(&overlay.prospective.top),
			vec![
				(vec![1], OverlayedValue { value: Some(vec![8]),
				 extrinsics: vec![4].into_iter().collect() }),
				(vec![3], OverlayedValue { value: Some(vec![7]),
				 extrinsics: vec![3].into_iter().collect() }),
			].into_iter().collect());

		overlay.commit_prospective();

		assert_eq!(strip_extrinsic_index(&overlay.committed.top),
			vec![
				(vec![1], OverlayedValue { value: Some(vec![8]),
				 extrinsics: vec![0, 2, 4].into_iter().collect() }),
				(vec![3], OverlayedValue { value: Some(vec![7]),
				 extrinsics: vec![1, 3].into_iter().collect() }),
				(vec![100], OverlayedValue { value: Some(vec![101]),
				 extrinsics: vec![NO_EXTRINSIC_INDEX].into_iter().collect() }),
			].into_iter().collect());

		assert_eq!(overlay.prospective,
			Default::default());
	}

	#[test]
	fn next_storage_key_change_works() {
		let mut overlay = OverlayedChanges::default();
		overlay.set_storage(vec![20], Some(vec![20]));
		overlay.set_storage(vec![30], Some(vec![30]));
		overlay.set_storage(vec![40], Some(vec![40]));
		overlay.commit_prospective();
		overlay.set_storage(vec![10], Some(vec![10]));
		overlay.set_storage(vec![30], None);

		// next_prospective < next_committed
		let next_to_5 = overlay.next_storage_key_change(&[5]).unwrap();
		assert_eq!(next_to_5.0.to_vec(), vec![10]);
		assert_eq!(next_to_5.1.value, Some(vec![10]));

		// next_committed < next_prospective
		let next_to_10 = overlay.next_storage_key_change(&[10]).unwrap();
		assert_eq!(next_to_10.0.to_vec(), vec![20]);
		assert_eq!(next_to_10.1.value, Some(vec![20]));

		// next_committed == next_prospective
		let next_to_20 = overlay.next_storage_key_change(&[20]).unwrap();
		assert_eq!(next_to_20.0.to_vec(), vec![30]);
		assert_eq!(next_to_20.1.value, None);

		// next_committed, no next_prospective
		let next_to_30 = overlay.next_storage_key_change(&[30]).unwrap();
		assert_eq!(next_to_30.0.to_vec(), vec![40]);
		assert_eq!(next_to_30.1.value, Some(vec![40]));

		overlay.set_storage(vec![50], Some(vec![50]));
		// next_prospective, no next_committed
		let next_to_40 = overlay.next_storage_key_change(&[40]).unwrap();
		assert_eq!(next_to_40.0.to_vec(), vec![50]);
		assert_eq!(next_to_40.1.value, Some(vec![50]));
	}

	#[test]
	fn next_child_storage_key_change_works() {
		let child_info = ChildInfo::new_default(b"Child1");
		let child_info = &child_info;
		let child = child_info.storage_key();
		let mut overlay = OverlayedChanges::default();
		overlay.set_child_storage(child_info, vec![20], Some(vec![20]));
		overlay.set_child_storage(child_info, vec![30], Some(vec![30]));
		overlay.set_child_storage(child_info, vec![40], Some(vec![40]));
		overlay.commit_prospective();
		overlay.set_child_storage(child_info, vec![10], Some(vec![10]));
		overlay.set_child_storage(child_info, vec![30], None);

		// next_prospective < next_committed
		let next_to_5 = overlay.next_child_storage_key_change(child, &[5]).unwrap();
		assert_eq!(next_to_5.0.to_vec(), vec![10]);
		assert_eq!(next_to_5.1.value, Some(vec![10]));

		// next_committed < next_prospective
		let next_to_10 = overlay.next_child_storage_key_change(child, &[10]).unwrap();
		assert_eq!(next_to_10.0.to_vec(), vec![20]);
		assert_eq!(next_to_10.1.value, Some(vec![20]));

		// next_committed == next_prospective
		let next_to_20 = overlay.next_child_storage_key_change(child, &[20]).unwrap();
		assert_eq!(next_to_20.0.to_vec(), vec![30]);
		assert_eq!(next_to_20.1.value, None);

		// next_committed, no next_prospective
		let next_to_30 = overlay.next_child_storage_key_change(child, &[30]).unwrap();
		assert_eq!(next_to_30.0.to_vec(), vec![40]);
		assert_eq!(next_to_30.1.value, Some(vec![40]));

		overlay.set_child_storage(child_info, vec![50], Some(vec![50]));
		// next_prospective, no next_committed
		let next_to_40 = overlay.next_child_storage_key_change(child, &[40]).unwrap();
		assert_eq!(next_to_40.0.to_vec(), vec![50]);
		assert_eq!(next_to_40.1.value, Some(vec![50]));
	}
}
