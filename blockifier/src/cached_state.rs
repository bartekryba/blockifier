use std::collections::HashMap;

use anyhow::{Context, Result};
use starknet_api::core::{ClassHash, ContractAddress, Nonce};
use starknet_api::hash::StarkFelt;
use starknet_api::state::StorageKey;

#[cfg(test)]
#[path = "cached_state_test.rs"]
mod test;

/// Caches read and write requests.
///
/// Writer functionality is built-in, whereas Reader functionality is injected through
/// initialization.
#[derive(Default)]
pub struct CachedState<SR: StateReader> {
    pub state_reader: SR,
    // Invariant: the cache should remain private.
    cache: StateCache,
}

impl<SR: StateReader> CachedState<SR> {
    pub fn new(state_reader: SR) -> Self {
        Self { state_reader, cache: StateCache::default() }
    }

    pub fn get_storage_at(
        &mut self,
        contract_address: ContractAddress,
        key: StorageKey,
    ) -> Result<&StarkFelt> {
        if self.cache.get_storage_at(contract_address, key).is_none() {
            let storage_value = self.state_reader.get_storage_at(contract_address, key)?;
            self.cache.set_storage_initial_values(contract_address, key, storage_value);
        }

        self.cache.get_storage_at(contract_address, key).with_context(|| {
            format!("Cannot retrieve '{contract_address:?}' and '{key:?}' from the cache.")
        })
    }

    pub fn set_storage_at(
        &mut self,
        contract_address: ContractAddress,
        key: StorageKey,
        value: StarkFelt,
    ) {
        self.cache.set_storage_writes(contract_address, key, value);
    }
}

/// A read-only API for accessing StarkNet global state.
pub trait StateReader {
    /// Returns the storage value under the given key in the given contract instance (represented by
    /// its address).
    fn get_storage_at(
        &self,
        contract_address: ContractAddress,
        key: StorageKey,
    ) -> Result<StarkFelt>;

    /// Returns the nonce of the given contract instance.
    fn get_nonce_at(&self, _contract_address: ContractAddress) -> Result<Nonce> {
        unimplemented!();
    }

    /// Returns the class hash of the contract class at the given contract instance;
    /// uninitialized class hash, if the address is unassigned.
    fn get_class_hash_at(&self, _contract_address: ContractAddress) -> Result<ClassHash> {
        unimplemented!();
    }
}

type ContractStorageKey = (ContractAddress, StorageKey);

/// A simple implementation of `StateReader` using `HashMap`s for storage.
#[derive(Default)]
pub struct DictStateReader {
    pub contract_storage_key_to_value: HashMap<ContractStorageKey, StarkFelt>,
}

impl StateReader for DictStateReader {
    fn get_storage_at(
        &self,
        contract_address: ContractAddress,
        key: StorageKey,
    ) -> Result<StarkFelt> {
        let contract_storage_key = (contract_address, key);
        let value = self
            .contract_storage_key_to_value
            .get(&contract_storage_key)
            .copied()
            .unwrap_or_else(default_storage_value);
        Ok(value)
    }
}

/// Caches read and write requests.
// Invariant: cannot delete keys from fields.
#[derive(Default)]
struct StateCache {
    // Reader's cached information; initial values, read before any write operation (per cell).
    _nonce_initial_values: HashMap<ContractAddress, Nonce>,
    _class_hash_initial_values: HashMap<ContractAddress, ClassHash>,
    storage_initial_values: HashMap<ContractStorageKey, StarkFelt>,

    // Writer's cached information.
    _nonce_writes: HashMap<ContractAddress, Nonce>,
    _class_hash_writes: HashMap<ContractAddress, ClassHash>,
    storage_writes: HashMap<ContractStorageKey, StarkFelt>,
}

impl StateCache {
    fn get_storage_at(
        &self,
        contract_address: ContractAddress,
        key: StorageKey,
    ) -> Option<&StarkFelt> {
        let contract_storage_key = (contract_address, key);
        self.storage_writes
            .get(&contract_storage_key)
            .or_else(|| self.storage_initial_values.get(&contract_storage_key))
    }

    pub fn set_storage_initial_values(
        &mut self,
        contract_address: ContractAddress,
        key: StorageKey,
        value: StarkFelt,
    ) {
        let contract_storage_key = (contract_address, key);
        self.storage_initial_values.insert(contract_storage_key, value);
    }

    fn set_storage_writes(
        &mut self,
        contract_address: ContractAddress,
        key: StorageKey,
        value: StarkFelt,
    ) {
        let contract_storage_key = (contract_address, key);
        self.storage_writes.insert(contract_storage_key, value);
    }
}

fn uninitialized_felt() -> StarkFelt {
    StarkFelt::default()
}

fn default_storage_value() -> StarkFelt {
    uninitialized_felt()
}