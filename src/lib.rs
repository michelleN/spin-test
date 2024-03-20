#[allow(warnings)]
mod bindings;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use bindings::exports::fermyon::spin::key_value;

struct Component;

impl key_value::Guest for Component {
    type Store = KeyValueStore;
}

#[derive(Debug, Default, Clone)]
struct KeyValueStore {
    inner: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl key_value::GuestStore for KeyValueStore {
    fn open(label: String) -> Result<key_value::Store, key_value::Error> {
        static STORES: std::sync::OnceLock<Mutex<HashMap<String, KeyValueStore>>> =
            std::sync::OnceLock::new();
        let mut stores = STORES.get_or_init(|| Default::default()).lock().unwrap();
        let key_value = stores.entry(label).or_default();
        Ok(key_value::Store::new(key_value.clone()))
    }

    fn get(&self, key: String) -> Result<Option<Vec<u8>>, key_value::Error> {
        Ok(self.inner.read().unwrap().get(&key).cloned())
    }

    fn set(&self, key: String, value: Vec<u8>) -> Result<(), key_value::Error> {
        self.inner.write().unwrap().insert(key, value);
        Ok(())
    }

    fn delete(&self, key: String) -> Result<(), key_value::Error> {
        let _ = self.inner.write().unwrap().remove(&key);
        todo!()
    }

    fn exists(&self, key: String) -> Result<bool, key_value::Error> {
        Ok(self.inner.read().unwrap().contains_key(&key))
    }

    fn get_keys(&self) -> Result<Vec<String>, key_value::Error> {
        Ok(self.inner.read().unwrap().keys().cloned().collect())
    }
}

bindings::export!(Component with_types_in bindings);