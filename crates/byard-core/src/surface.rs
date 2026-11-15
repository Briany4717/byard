//! Surfaces: what a program draws into (RFC-0068 §5, §6).
//!
//! A Byard program has always drawn into one window. RFC-0068 lets it own
//! several surfaces (windows, panels, popups) that share one interpreter, and
//! this module holds the three small types the engine and the relay use to tell
//! them apart:
//!
//! - [`SurfaceId`], which surface an event, a frame or a change is about.
//!   [`SurfaceId::MAIN`] is the one implicit window every program has today and
//!   the only one anything creates yet.
//! - [`SurfaceSpec`], what a surface is made of. Empty for now: the step that
//!   adds `Window` fills it in.
//! - [`SurfaceChange`], a request from the logic thread to the main thread to
//!   create, reconfigure or destroy a surface, because only the main thread may
//!   touch the platform's windows.
//!
//! None of it costs a single-surface program anything: `MAIN` is a constant, and
//! the relay keeps its slot inline.

/// Which surface something belongs to.
///
/// A small integer, `Copy` and cheap to compare. `0` is [`SurfaceId::MAIN`]; a
/// program that never creates another surface never sees any other value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(u32);

impl SurfaceId {
    /// The one implicit window of a single-surface program, and the first
    /// surface of any other.
    pub const MAIN: Self = Self(0);

    /// The surface with this number. `0` is [`SurfaceId::MAIN`].
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The number this id was made from.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Whether this is [`SurfaceId::MAIN`].
    #[must_use]
    pub const fn is_main(self) -> bool {
        self.0 == 0
    }
}

/// What a surface is made of: its kind, its size, its title, everything the
/// platform needs to create it.
///
/// Empty today. The step that adds `Window` (RFC-0068 §2) gives it fields, and
/// the type is `#[non_exhaustive]` so that does not break anything that already
/// builds one with [`SurfaceSpec::new`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SurfaceSpec {}

impl SurfaceSpec {
    /// A spec with nothing set.
    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }
}

/// A request from the logic thread to the main thread about one surface.
///
/// The logic thread cannot create a platform surface (only the main thread may),
/// so it appends one of these to the relay
/// ([`Relay::push_surface_change`](crate::relay::Relay::push_surface_change))
/// and wakes the main thread, which applies them in order
/// ([`Relay::drain_surface_changes`](crate::relay::Relay::drain_surface_changes)).
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceChange {
    /// A surface came into being and needs a platform surface made for it.
    Create {
        /// The new surface.
        id: SurfaceId,
        /// What it is made of.
        spec: SurfaceSpec,
    },
    /// A surface's reactive attributes changed.
    Configure {
        /// The surface.
        id: SurfaceId,
        /// What it is made of now.
        spec: SurfaceSpec,
    },
    /// A surface is gone and its platform surface is to be destroyed.
    Destroy {
        /// The surface.
        id: SurfaceId,
    },
}

impl SurfaceChange {
    /// The surface this change is about.
    #[must_use]
    pub const fn id(&self) -> SurfaceId {
        match self {
            Self::Create { id, .. } | Self::Configure { id, .. } | Self::Destroy { id } => *id,
        }
    }
}

/// One `T` per surface, for the state a host keeps for each of its surfaces (an
/// accessibility adapter, a platform window).
///
/// The value of [`SurfaceId::MAIN`] is held inline and the others in a vector
/// that allocates only when a second surface is inserted, so a single-surface
/// program pays one `Option` and no lookup (RFC-0068 §10).
#[derive(Debug)]
pub struct PerSurface<T> {
    main: Option<T>,
    others: Vec<(SurfaceId, T)>,
}

impl<T> Default for PerSurface<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PerSurface<T> {
    /// An empty map.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            main: None,
            others: Vec::new(),
        }
    }

    /// The value for `id`, if it has one.
    #[must_use]
    pub fn get(&self, id: SurfaceId) -> Option<&T> {
        if id.is_main() {
            self.main.as_ref()
        } else {
            self.others.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
        }
    }

    /// The value for `id`, mutably.
    pub fn get_mut(&mut self, id: SurfaceId) -> Option<&mut T> {
        if id.is_main() {
            self.main.as_mut()
        } else {
            self.others
                .iter_mut()
                .find(|(i, _)| *i == id)
                .map(|(_, v)| v)
        }
    }

    /// Sets the value for `id`, returning the one it replaced.
    pub fn insert(&mut self, id: SurfaceId, value: T) -> Option<T> {
        if id.is_main() {
            return self.main.replace(value);
        }
        if let Some((_, slot)) = self.others.iter_mut().find(|(i, _)| *i == id) {
            return Some(std::mem::replace(slot, value));
        }
        self.others.push((id, value));
        None
    }

    /// Removes and returns the value for `id`.
    pub fn remove(&mut self, id: SurfaceId) -> Option<T> {
        if id.is_main() {
            return self.main.take();
        }
        let index = self.others.iter().position(|(i, _)| *i == id)?;
        Some(self.others.swap_remove(index).1)
    }

    /// How many surfaces have a value.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.main.is_some()) + self.others.len()
    }

    /// Whether no surface has a value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every value with its surface, [`SurfaceId::MAIN`]'s first.
    pub fn iter(&self) -> impl Iterator<Item = (SurfaceId, &T)> {
        self.main
            .iter()
            .map(|v| (SurfaceId::MAIN, v))
            .chain(self.others.iter().map(|(i, v)| (*i, v)))
    }

    /// Every value mutably, [`SurfaceId::MAIN`]'s first.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.main
            .iter_mut()
            .chain(self.others.iter_mut().map(|(_, v)| v))
    }

    /// Whether the vector for the other surfaces has allocated. `false` for a
    /// program that never had a second surface, which is what the cost
    /// assertion reads.
    #[must_use]
    pub fn has_allocated(&self) -> bool {
        self.others.capacity() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_surface_is_the_default_and_the_first() {
        assert_eq!(SurfaceId::default(), SurfaceId::MAIN);
        assert_eq!(SurfaceId::new(0), SurfaceId::MAIN);
        assert!(SurfaceId::MAIN.is_main());
        assert!(!SurfaceId::new(1).is_main());
        assert_eq!(SurfaceId::new(7).index(), 7);
    }

    #[test]
    fn a_map_with_only_main_never_allocates() {
        let mut map = PerSurface::new();
        assert!(map.is_empty());
        assert_eq!(map.insert(SurfaceId::MAIN, "bar"), None);
        assert_eq!(map.insert(SurfaceId::MAIN, "dock"), Some("bar"));
        assert_eq!(map.get(SurfaceId::MAIN), Some(&"dock"));
        assert_eq!(map.len(), 1);
        assert!(!map.has_allocated(), "MAIN lives inline");
        assert_eq!(map.remove(SurfaceId::MAIN), Some("dock"));
        assert!(map.is_empty());
    }

    #[test]
    fn a_map_keeps_one_value_per_surface() {
        let mut map = PerSurface::new();
        map.insert(SurfaceId::MAIN, 0);
        map.insert(SurfaceId::new(2), 2);
        map.insert(SurfaceId::new(5), 5);
        assert_eq!(map.len(), 3);
        assert!(map.has_allocated());
        assert_eq!(map.get(SurfaceId::new(2)), Some(&2));
        assert_eq!(map.get(SurfaceId::new(3)), None);
        *map.get_mut(SurfaceId::new(5)).unwrap() += 10;
        assert_eq!(map.remove(SurfaceId::new(2)), Some(2));
        let all: Vec<_> = map.iter().map(|(i, v)| (i.index(), *v)).collect();
        assert_eq!(all, vec![(0, 0), (5, 15)]);
        for v in map.values_mut() {
            *v += 1;
        }
        assert_eq!(map.get(SurfaceId::MAIN), Some(&1));
    }

    #[test]
    fn a_change_names_its_surface() {
        let id = SurfaceId::new(3);
        assert_eq!(SurfaceChange::Destroy { id }.id(), id);
        assert_eq!(
            SurfaceChange::Create {
                id,
                spec: SurfaceSpec::new()
            }
            .id(),
            id
        );
    }
}
