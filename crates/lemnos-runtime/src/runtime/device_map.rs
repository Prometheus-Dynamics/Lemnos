use lemnos_core::DeviceId;

/// A map from device ID to `V`, kept as a vector sorted by ID.
///
/// The runtime's per-device state (bindings, cached states, desired bindings)
/// is control-plane sized, so a binary search over a sorted vector serves it.
/// A `BTreeMap` instead links its node code once per key and value type: four
/// of them, with their split, merge and iterator paths, cost about as much as
/// the rest of the device bookkeeping. Iteration stays in ID order, as
/// `BTreeMap` iterates, so nothing observable changes.
#[derive(Debug)]
pub(crate) struct DeviceMap<V> {
    entries: Vec<(DeviceId, V)>,
}

impl<V> Default for DeviceMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<V> DeviceMap<V> {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn get(&self, device_id: &DeviceId) -> Option<&V> {
        self.position(device_id)
            .ok()
            .map(|index| &self.entries[index].1)
    }

    pub(crate) fn contains_key(&self, device_id: &DeviceId) -> bool {
        self.position(device_id).is_ok()
    }

    /// Inserts `value`, returning the value it replaced, if any.
    pub(crate) fn insert(&mut self, device_id: DeviceId, value: V) -> Option<V> {
        match self.position(&device_id) {
            Ok(index) => Some(std::mem::replace(&mut self.entries[index].1, value)),
            Err(index) => {
                self.entries.insert(index, (device_id, value));
                None
            }
        }
    }

    pub(crate) fn remove(&mut self, device_id: &DeviceId) -> Option<V> {
        self.position(device_id)
            .ok()
            .map(|index| self.entries.remove(index).1)
    }

    /// The entries in ID order, consuming the map.
    pub(crate) fn into_entries(self) -> Vec<(DeviceId, V)> {
        self.entries
    }

    fn position(&self, device_id: &DeviceId) -> Result<usize, usize> {
        self.entries.binary_search_by(|(id, _)| id.cmp(device_id))
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceMap;
    use lemnos_core::DeviceId;

    fn id(name: &str) -> DeviceId {
        DeviceId::new(name).expect("valid id")
    }

    #[test]
    fn keeps_entries_in_id_order_and_replaces_in_place() {
        let mut map = DeviceMap::default();
        assert_eq!(map.insert(id("gpio-b"), 2), None);
        assert_eq!(map.insert(id("gpio-a"), 1), None);
        assert_eq!(map.insert(id("gpio-c"), 3), None);
        assert_eq!(map.insert(id("gpio-b"), 20), Some(2));

        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&id("gpio-b")), Some(&20));
        assert!(map.contains_key(&id("gpio-a")));
        assert!(!map.contains_key(&id("gpio-z")));
        let keys: Vec<_> = map
            .into_entries()
            .into_iter()
            .map(|(key, _)| key.as_str().to_string())
            .collect();
        assert_eq!(keys, ["gpio-a", "gpio-b", "gpio-c"]);
    }

    #[test]
    fn remove_returns_the_value_and_clear_empties() {
        let mut map = DeviceMap::default();
        map.insert(id("gpio-a"), 1);
        map.insert(id("gpio-b"), 2);
        assert_eq!(map.remove(&id("gpio-a")), Some(1));
        assert_eq!(map.remove(&id("gpio-a")), None);
        map.clear();
        assert_eq!(map.len(), 0);
    }
}
