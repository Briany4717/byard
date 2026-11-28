//! `Map<K, V>` and `Set<T>` at run time: the entries in the order they were
//! added, shared, with an index that finds a key without a search.
//!
//! A map or a set is a value, so writing one makes another (`m.set(k, v)`),
//! and reading one is cheap: the entries are behind an [`Arc`], so a copy of a
//! value that holds a map, which every read of a `var` makes, copies a
//! pointer. Only a write copies the entries, once.
//!
//! The index is a hint, not the answer. Keys are equal as `==` says they are
//! ([`structural_eq`]): an `Int` and a `Float` of the same value are one key,
//! and two `Float`s within `f64::EPSILON` are one too, which no hash can say
//! exactly. So a key is hashed to a bucket that every key equal to it is
//! sure to share, and the bucket's entries are compared with
//! [`structural_eq`]. Whole numbers, text, `Bool`s, enum cases, lists and
//! records each get a bucket of their own, which is what makes a lookup by
//! one of them a hash and not a search; a `Float` with a fraction shares one
//! bucket with every other, and is found by comparing them.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::env::Value;

/// Structural (element/field-wise) equality for lists and records (RFC-0027
/// §1), with numeric Int↔Float promotion at the leaves.
pub fn structural_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < f64::EPSILON,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
            (*x as f64 - *y).abs() < f64::EPSILON
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        // A set is the same set in any order, and a map the same map.
        (Value::Set(xs), Value::Set(ys)) => {
            xs.len() == ys.len() && xs.items().iter().all(|x| ys.contains(x))
        }
        (Value::Map(xs), Value::Map(ys)) => {
            xs.len() == ys.len()
                && xs
                    .pairs()
                    .iter()
                    .all(|(k, vx)| ys.get(k).is_some_and(|vy| structural_eq(vx, vy)))
        }
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| structural_eq(x, y))
        }
        // Fields are compared by name, in whatever order they were written.
        (Value::Record(xs), Value::Record(ys)) => {
            xs.len() == ys.len()
                && xs.iter().all(|(kx, vx)| {
                    ys.iter()
                        .find(|(ky, _)| ky == kx)
                        .is_some_and(|(_, vy)| structural_eq(vx, vy))
                })
        }
        _ => false,
    }
}

/// The bucket of `value`: equal by [`structural_eq`] means one bucket.
fn bucket(value: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    feed(value, &mut h);
    h.finish()
}

fn feed(value: &Value, h: &mut impl Hasher) {
    match value {
        Value::Int(n) => {
            0u8.hash(h);
            n.hash(h);
        }
        // A whole number is the `Int` it equals; any other `Float` is in the
        // one bucket of fractions (see the module's comment).
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        Value::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 {
                0u8.hash(h);
                (*f as i64).hash(h);
            } else {
                1u8.hash(h);
            }
        }
        Value::Bool(b) => {
            2u8.hash(h);
            b.hash(h);
        }
        Value::Str(s) => {
            3u8.hash(h);
            s.hash(h);
        }
        Value::Unit => 4u8.hash(h),
        Value::List(xs) => {
            5u8.hash(h);
            xs.len().hash(h);
            for x in xs {
                feed(x, h);
            }
        }
        // Fields in any order: their names and buckets, summed.
        Value::Record(fields) => {
            6u8.hash(h);
            let mut sum = 0u64;
            for (name, v) in fields {
                let mut one = std::collections::hash_map::DefaultHasher::new();
                name.as_str().hash(&mut one);
                feed(v, &mut one);
                sum = sum.wrapping_add(one.finish());
            }
            sum.hash(h);
        }
        // Equal in any order: only the size is sure to agree.
        Value::Map(m) => {
            7u8.hash(h);
            m.len().hash(h);
        }
        Value::Set(s) => {
            8u8.hash(h);
            s.len().hash(h);
        }
        // Nothing else is a key anyone writes; one bucket, compared.
        _ => 9u8.hash(h),
    }
}

/// The positions of the entries in each bucket.
type Index = HashMap<u64, Vec<usize>>;

fn index_of<'a>(keys: impl Iterator<Item = &'a Value>) -> Index {
    let mut index: Index = HashMap::new();
    for (i, k) in keys.enumerate() {
        index.entry(bucket(k)).or_default().push(i);
    }
    index
}

/// The entries of a `Map`, in the order their keys were first added.
#[derive(Clone, Default)]
pub struct MapData(Arc<(Vec<(Value, Value)>, Index)>);

impl MapData {
    /// A map of `pairs`, a later value of a key replacing an earlier one.
    #[must_use]
    pub fn from_pairs(pairs: Vec<(Value, Value)>) -> Self {
        let mut out: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
        let mut index: Index = HashMap::new();
        for (k, v) in pairs {
            let b = bucket(&k);
            let slot = index.entry(b).or_default();
            if let Some(&i) = slot.iter().find(|&&i| structural_eq(&out[i].0, &k)) {
                out[i].1 = v;
            } else {
                slot.push(out.len());
                out.push((k, v));
            }
        }
        Self(Arc::new((out, index)))
    }

    /// The map of each key to the list of the items given with it, in the
    /// order the keys were first met and, in each list, the items were:
    /// what `xs.groupBy(x => key)` gives.
    #[must_use]
    pub fn grouped(keyed: Vec<(Value, Value)>) -> Self {
        let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
        let mut index: Index = HashMap::new();
        for (k, item) in keyed {
            let slot = index.entry(bucket(&k)).or_default();
            if let Some(&i) = slot.iter().find(|&&i| structural_eq(&groups[i].0, &k)) {
                groups[i].1.push(item);
            } else {
                slot.push(groups.len());
                groups.push((k, vec![item]));
            }
        }
        let pairs = groups
            .into_iter()
            .map(|(k, items)| (k, Value::List(items)))
            .collect();
        Self(Arc::new((pairs, index)))
    }

    /// The entries, in order.
    #[must_use]
    pub fn pairs(&self) -> &[(Value, Value)] {
        &self.0.0
    }

    /// How many keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.0.len()
    }

    /// Whether it has no key.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }

    fn position(&self, key: &Value) -> Option<usize> {
        let (pairs, index) = &*self.0;
        index
            .get(&bucket(key))?
            .iter()
            .copied()
            .find(|&i| structural_eq(&pairs[i].0, key))
    }

    /// The value of `key`, if it has one.
    #[must_use]
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.position(key).map(|i| &self.0.0[i].1)
    }

    /// The map with `key` set to `value`: replaced where it was, or added last.
    #[must_use]
    pub fn set(&self, key: Value, value: Value) -> Self {
        let (mut pairs, mut index) = (*self.0).clone();
        if let Some(i) = self.position(&key) {
            pairs[i].1 = value;
        } else {
            index.entry(bucket(&key)).or_default().push(pairs.len());
            pairs.push((key, value));
        }
        Self(Arc::new((pairs, index)))
    }

    /// The map without `key`.
    #[must_use]
    pub fn remove(&self, key: &Value) -> Self {
        let Some(at) = self.position(key) else {
            return self.clone();
        };
        let mut pairs = self.0.0.clone();
        pairs.remove(at);
        let index = index_of(pairs.iter().map(|(k, _)| k));
        Self(Arc::new((pairs, index)))
    }
}

impl std::fmt::Debug for MapData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.pairs().iter().map(|(k, v)| (k, v)))
            .finish()
    }
}

/// Two maps are equal as values with the same entries in the same order are.
impl PartialEq for MapData {
    fn eq(&self, other: &Self) -> bool {
        self.pairs() == other.pairs()
    }
}

/// The items of a `Set`, each once, in the order they were first added.
#[derive(Clone, Default)]
pub struct SetData(Arc<(Vec<Value>, Index)>);

impl SetData {
    /// A set of `items`, each kept the first time it is met.
    #[must_use]
    pub fn from_items(items: Vec<Value>) -> Self {
        let mut out: Vec<Value> = Vec::with_capacity(items.len());
        let mut index: Index = HashMap::new();
        for item in items {
            let slot = index.entry(bucket(&item)).or_default();
            if !slot.iter().any(|&i| structural_eq(&out[i], &item)) {
                slot.push(out.len());
                out.push(item);
            }
        }
        Self(Arc::new((out, index)))
    }

    /// The items, in order.
    #[must_use]
    pub fn items(&self) -> &[Value] {
        &self.0.0
    }

    /// How many items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.0.len()
    }

    /// Whether it has no item.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }

    /// Whether `item` is in it.
    #[must_use]
    pub fn contains(&self, item: &Value) -> bool {
        let (items, index) = &*self.0;
        index
            .get(&bucket(item))
            .is_some_and(|slot| slot.iter().any(|&i| structural_eq(&items[i], item)))
    }

    /// The set with `item` in it.
    #[must_use]
    pub fn add(&self, item: Value) -> Self {
        if self.contains(&item) {
            return self.clone();
        }
        let (mut items, mut index) = (*self.0).clone();
        index.entry(bucket(&item)).or_default().push(items.len());
        items.push(item);
        Self(Arc::new((items, index)))
    }

    /// The set without `item`.
    #[must_use]
    pub fn remove(&self, item: &Value) -> Self {
        if !self.contains(item) {
            return self.clone();
        }
        let items: Vec<Value> = self
            .items()
            .iter()
            .filter(|x| !structural_eq(x, item))
            .cloned()
            .collect();
        let index = index_of(items.iter());
        Self(Arc::new((items, index)))
    }
}

impl std::fmt::Debug for SetData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.items()).finish()
    }
}

impl PartialEq for SetData {
    fn eq(&self, other: &Self) -> bool {
        self.items() == other.items()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Value {
        Value::Str(x.to_string())
    }

    #[test]
    fn a_map_keeps_its_order_and_a_later_value_of_a_key() {
        let m = MapData::from_pairs(vec![(s("b"), Value::Int(1)), (s("a"), Value::Int(2))]);
        let m = m.set(s("b"), Value::Int(9)).set(s("c"), Value::Int(3));
        let keys: Vec<&Value> = m.pairs().iter().map(|(k, _)| k).collect();
        assert_eq!(keys, [&s("b"), &s("a"), &s("c")]);
        assert_eq!(m.get(&s("b")), Some(&Value::Int(9)));
        assert_eq!(m.remove(&s("a")).len(), 2);
        assert_eq!(m.remove(&s("a")).get(&s("c")), Some(&Value::Int(3)));
    }

    #[test]
    fn a_key_is_found_as_equality_says_it_is() {
        let m = MapData::from_pairs(vec![
            (Value::Int(1), s("one")),
            (Value::Float(0.5), s("half")),
        ]);
        // An `Int` and the `Float` of its value are one key, and so are two
        // floats within epsilon.
        assert_eq!(m.get(&Value::Float(1.0)), Some(&s("one")));
        assert_eq!(
            m.get(&Value::Float(0.5 + f64::EPSILON / 4.0)),
            Some(&s("half"))
        );
        assert_eq!(m.get(&Value::Int(2)), None);
        let rec = |a: i64, b: &str| {
            Value::Record(vec![
                (crate::symbol::Symbol::intern("a"), Value::Int(a)),
                (crate::symbol::Symbol::intern("b"), s(b)),
            ])
        };
        let swapped = Value::Record(vec![
            (crate::symbol::Symbol::intern("b"), s("x")),
            (crate::symbol::Symbol::intern("a"), Value::Int(1)),
        ]);
        let m = MapData::from_pairs(vec![(rec(1, "x"), Value::Bool(true))]);
        assert_eq!(m.get(&swapped), Some(&Value::Bool(true)));
    }

    #[test]
    fn a_set_holds_each_item_once() {
        let set = SetData::from_items(vec![Value::Int(1), Value::Float(1.0), Value::Int(2)]);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&Value::Int(2)));
        assert_eq!(set.add(Value::Int(2)).len(), 2);
        assert_eq!(set.add(Value::Int(3)).len(), 3);
        assert!(!set.remove(&Value::Int(1)).contains(&Value::Float(1.0)));
    }

    #[test]
    fn a_lookup_in_a_large_map_is_a_hash() {
        let n = 20_000;
        let m = MapData::from_pairs((0..n).map(|i| (Value::Int(i), Value::Int(i * 2))).collect());
        // Every key in its own bucket: the index holds one position each.
        assert!(m.0.1.values().all(|slot| slot.len() == 1));
        assert_eq!(m.get(&Value::Int(n - 1)), Some(&Value::Int((n - 1) * 2)));
    }
}
