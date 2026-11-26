//! What extensions keep for their pages' pictures ([`peersus_api::pictures`]): finished pixels, each under an id of the
//! extension's own, which its pages place with `Node::Picture`.
//!
//! WHO MADE THEM. The extension, in its own process: the application never decodes a picture for it. It reads what the extension
//! put in its private storage, checks that it is exactly the size the extension said (a side no longer than
//! [`peersus_api::pictures::MAX_EDGE`], `width * height * 4` bytes), and keeps it as it is.
//!
//! WHAT A WINDOW IS TOLD. A page that places a picture names it by id; the owner writes the kept picture's digest into that
//! element, and a window that does not hold the digest asks the owner for the pixels once, as it asks for a device's picture.
//! One extension never places, reads or forgets another's pictures: ids are looked up in the caller's own set.
//!
//! WHAT BOUNDS IT. An extension keeps at most [`MAX_KEPT`] pictures and [`MAX_KEPT_BYTES`] of pixels; a `keep` past either is
//! refused and changes nothing. What an extension keeps goes with it when it stops, restarts, is turned off or is removed: a
//! `keep` still reading when that happens is not let in afterwards (its generation is gone).
use peersus_api::pictures::{Forget, MAX_KEPT, MAX_KEPT_BYTES};
use peersus_core::{
    plugins::PluginId,
    profile::{Digest, Kept},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Default)]
struct Set {
    pictures: BTreeMap<String, (Digest, Arc<Kept>)>,
    bytes: usize,
}

/// The pictures of every extension, each in a set of its own.
#[derive(Default)]
pub(crate) struct KeptPictures {
    /// Only extensions that keep something: a set that was emptied goes.
    sets: BTreeMap<PluginId, Set>,
    /// Changes whenever an extension's pictures are let go of all at once, so a read begun before is not kept after.
    generations: BTreeMap<PluginId, u64>,
    /// Something changed that pages place: the owner fills its pages in again and tells the windows.
    changed: bool,
}

impl KeptPictures {
    /// What a `keep` begun now must still match when its pixels arrive.
    pub(crate) fn generation(&self, plugin: &PluginId) -> u64 {
        self.generations.get(plugin).copied().unwrap_or(0)
    }

    /// Keep `picture` for `plugin` under `id`, or replace the one under that id, if `generation` is still the extension's.
    /// Keeping what is kept already changes nothing. A refusal keeps the previous set.
    pub(crate) fn keep(
        &mut self,
        plugin: &PluginId,
        generation: u64,
        id: String,
        picture: Kept,
    ) -> Result<(), &'static str> {
        if generation != self.generation(plugin) {
            return Err("The extension stopped while the picture was read");
        }
        let digest = picture.digest();
        let set = self.sets.get(plugin);
        let old = set.and_then(|set| set.pictures.get(&id));
        if old.is_some_and(|(kept, _)| *kept == digest) {
            return Ok(());
        }
        let count = set.map_or(0, |set| set.pictures.len()) + usize::from(old.is_none());
        let bytes = set.map_or(0, |set| set.bytes) - old.map_or(0, |(_, old)| old.rgba().len())
            + picture.rgba().len();
        if count > MAX_KEPT || bytes > MAX_KEPT_BYTES {
            return Err("This extension keeps as many pictures as it may");
        }
        let set = self.sets.entry(plugin.clone()).or_default();
        set.pictures.insert(id, (digest, Arc::new(picture)));
        set.bytes = bytes;
        self.changed = true;
        Ok(())
    }

    /// Let go of the pictures `forget` names that `plugin` keeps; the others are ignored.
    pub(crate) fn forget(&mut self, plugin: &PluginId, forget: &Forget) {
        let Some(set) = self.sets.get_mut(plugin) else {
            return;
        };
        for id in &forget.ids {
            if let Some((_, picture)) = set.pictures.remove(id) {
                set.bytes -= picture.rgba().len();
                self.changed = true;
            }
        }
        if set.pictures.is_empty() {
            self.sets.remove(plugin);
        }
    }

    /// Let go of everything `plugin` keeps, and of every `keep` it has begun.
    pub(crate) fn clear(&mut self, plugin: &PluginId) {
        let generation = self.generations.entry(plugin.clone()).or_default();
        *generation = generation.wrapping_add(1);
        if self.sets.remove(plugin).is_some() {
            self.changed = true;
        }
    }

    /// The digest of the picture `plugin` keeps under `id`.
    pub(crate) fn digest(&self, plugin: &PluginId, id: &str) -> Option<Digest> {
        Some(self.sets.get(plugin)?.pictures.get(id)?.0)
    }

    /// The pixels a window asks for by digest, whichever extension keeps them: a window learns a digest only from a page that
    /// places it.
    pub(crate) fn picture(&self, digest: &Digest) -> Option<Kept> {
        self.sets.values().find_map(|set| {
            set.pictures
                .values()
                .find(|(kept, _)| kept == digest)
                .map(|(_, picture)| Kept::clone(picture))
        })
    }

    /// Whether anything pages place changed since the last time this was asked.
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    fn picture(side: u32, seed: u8) -> Kept {
        Kept::new(side, side, vec![seed; (side * side * 4) as usize]).unwrap()
    }

    #[test]
    fn a_kept_picture_is_found_by_its_own_extension_and_id_and_by_digest() {
        let mut kept = KeptPictures::default();
        let (a, b) = (plugin("a"), plugin("b"));
        kept.keep(&a, 0, "thumb/1".into(), picture(4, 1)).unwrap();
        assert!(kept.take_changed());
        let digest = kept.digest(&a, "thumb/1").unwrap();
        assert_eq!(
            kept.digest(&b, "thumb/1"),
            None,
            "another extension's id named it"
        );
        assert_eq!(kept.picture(&digest), Some(picture(4, 1)));
        // The same pixels again change nothing a window would redraw for.
        kept.keep(&a, 0, "thumb/1".into(), picture(4, 1)).unwrap();
        assert!(!kept.take_changed());
    }

    #[test]
    fn past_the_count_or_the_bytes_a_keep_is_refused_and_the_set_stays() {
        let mut kept = KeptPictures::default();
        let a = plugin("a");
        for n in 0..MAX_KEPT {
            kept.keep(&a, 0, format!("p/{n}"), picture(1, 0)).unwrap();
        }
        assert!(kept.keep(&a, 0, "one-more".into(), picture(1, 0)).is_err());
        assert_eq!(kept.digest(&a, "one-more"), None);
        // Replacing one that is kept is not one more.
        kept.keep(&a, 0, "p/0".into(), picture(1, 9)).unwrap();

        let mut kept = KeptPictures::default();
        let full = MAX_KEPT_BYTES / (256 * 256 * 4);
        for n in 0..full {
            kept.keep(&a, 0, format!("big/{n}"), picture(256, 0))
                .unwrap();
        }
        assert!(kept.keep(&a, 0, "past".into(), picture(256, 0)).is_err());
        kept.forget(
            &a,
            &Forget {
                ids: vec!["big/0".into()],
            },
        );
        kept.keep(&a, 0, "past".into(), picture(256, 0)).unwrap();
    }

    #[test]
    fn what_was_read_before_the_extension_stopped_is_not_let_in_after() {
        let mut kept = KeptPictures::default();
        let a = plugin("a");
        let begun = kept.generation(&a);
        kept.keep(&a, begun, "x".into(), picture(2, 1)).unwrap();
        kept.clear(&a);
        assert!(kept.take_changed());
        assert_eq!(kept.digest(&a, "x"), None);
        assert!(kept.keep(&a, begun, "y".into(), picture(2, 1)).is_err());
        assert!(!kept.take_changed());
        kept.keep(&a, kept.generation(&a), "y".into(), picture(2, 1))
            .unwrap();
    }
}
