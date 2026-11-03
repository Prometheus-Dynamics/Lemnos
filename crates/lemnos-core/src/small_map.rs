//! A map kept as a sorted vector of pairs.
//!
//! Descriptor labels and properties, telemetry, realized configuration and
//! issue attributes hold a handful of entries each. A sorted vector keeps them
//! in key order (the same iteration order as a `BTreeMap`) with one small
//! code path per value type, where a B-tree instantiates node splitting,
//! merging and iterator code for every key and value type it is used with.
//! Keys are [`MapKey`]s, so the keys the probes emit (`"backend"`,
//! `"sysfs_path"`) are borrowed `'static` strings and allocate nothing.

use alloc::borrow::{Borrow, Cow};
use alloc::vec::Vec;
use core::fmt;
use core::mem;
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// The key type of every [`SmallMap`] in the model: borrowed for static names,
/// owned for names built at run time.
pub type MapKey = Cow<'static, str>;

pub struct SmallMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> SmallMap<K, V> {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> + ExactSizeIterator {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + ExactSizeIterator {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, v)| v)
    }

    pub fn retain<F: FnMut(&K, &mut V) -> bool>(&mut self, mut keep: F) {
        self.entries.retain_mut(|(k, v)| keep(k, v));
    }
}

impl<K: Ord, V> SmallMap<K, V> {
    fn position<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.binary_search_by(|(k, _)| k.borrow().cmp(key))
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.position(key).ok().map(|i| &self.entries[i].1)
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.position(key) {
            Ok(i) => Some(&mut self.entries[i].1),
            Err(_) => None,
        }
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.position(key).is_ok()
    }

    /// Replaces the value of an existing key (keeping the key) or inserts a
    /// new pair, returning the previous value.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.position(&key) {
            Ok(i) => Some(mem::replace(&mut self.entries[i].1, value)),
            Err(i) => {
                self.entries.insert(i, (key, value));
                None
            }
        }
    }

    /// The value for `key`, inserted as `V::default()` first when it is absent.
    pub fn entry_or_default(&mut self, key: K) -> &mut V
    where
        V: Default,
    {
        let index = match self.position(&key) {
            Ok(index) => index,
            Err(index) => {
                self.entries.insert(index, (key, V::default()));
                index
            }
        };
        &mut self.entries[index].1
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.position(key) {
            Ok(i) => Some(self.entries.remove(i).1),
            Err(_) => None,
        }
    }
}

impl<K: Ord + Borrow<Q>, Q: Ord + ?Sized, V> core::ops::Index<&Q> for SmallMap<K, V> {
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("no entry found for key")
    }
}

impl<K, V> Default for SmallMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone, V: Clone> Clone for SmallMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for SmallMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<K: Eq, V: Eq> Eq for SmallMap<K, V> {}

impl<K: PartialOrd, V: PartialOrd> PartialOrd for SmallMap<K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.entries.partial_cmp(&other.entries)
    }
}

impl<K: Ord, V: Ord> Ord for SmallMap<K, V> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.entries.cmp(&other.entries)
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for SmallMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(k, v)| (k, v)))
            .finish()
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for SmallMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        map.extend(iter);
        map
    }
}

impl<K: Ord, V> Extend<(K, V)> for SmallMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<K, V> IntoIterator for SmallMap<K, V> {
    type Item = (K, V);
    type IntoIter = alloc::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a SmallMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter =
        core::iter::Map<core::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

#[cfg(feature = "serde")]
impl<K: Serialize, V: Serialize> Serialize for SmallMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

#[cfg(feature = "serde")]
impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Deserialize<'de> for SmallMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K, V>(core::marker::PhantomData<(K, V)>);

        impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> de::Visitor<'de> for MapVisitor<K, V> {
            type Value = SmallMap<K, V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map")
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut map = SmallMap::new();
                while let Some((key, value)) = access.next_entry::<K, V>()? {
                    map.insert(key, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(MapVisitor(core::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn keeps_keys_sorted_and_replaces_in_place() {
        let mut map: SmallMap<MapKey, u32> = SmallMap::new();
        assert_eq!(map.insert("b".into(), 2), None);
        assert_eq!(map.insert("a".into(), 1), None);
        assert_eq!(map.insert("b".to_string().into(), 3), Some(2));
        let keys: Vec<&str> = map.keys().map(|k| k.as_ref()).collect();
        assert_eq!(keys, ["a", "b"]);
        assert_eq!(map.get("b"), Some(&3));
        assert_eq!(map.remove("a"), Some(1));
        assert_eq!(map.get("a"), None);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn collect_keeps_the_last_value_for_a_repeated_key() {
        let map: SmallMap<MapKey, u32> = [("z".into(), 1), ("a".into(), 2), ("z".into(), 3)]
            .into_iter()
            .collect();
        let pairs: Vec<(&str, u32)> = map.iter().map(|(k, v)| (k.as_ref(), *v)).collect();
        assert_eq!(pairs, [("a", 2), ("z", 3)]);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serializes_as_a_map_in_key_order() {
        let mut map: SmallMap<MapKey, u32> = SmallMap::new();
        map.insert("b".into(), 2);
        map.insert("a".into(), 1);
        let json = serde_json::to_string(&map).expect("serialize");
        assert_eq!(json, r#"{"a":1,"b":2}"#);
        let back: SmallMap<MapKey, u32> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, map);
    }
}
