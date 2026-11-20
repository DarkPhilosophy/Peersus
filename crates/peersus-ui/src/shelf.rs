//! What the window holds of other pictures, and the one rule about asking for more.
//!
//! WHY THE WINDOW KEEPS PICTURES AT ALL. A snapshot names a picture by its digest and never carries
//! it. A window that has not got that digest asks the owner for the pixels, once, and keeps them
//! under that digest, so that the next snapshot, and the one after, which name the same picture, ask
//! for nothing. This is the same arrangement as between two devices, one step closer in.
//!
//! WHAT IT DECIDES. Which digests must be asked for: those the snapshot names that the window neither
//! holds nor is already waiting for. And which pictures can be let go: those no snapshot names any
//! more. It knows no window and no owner, so it is tried without either.
//!
//! WHAT BOUNDS IT. Nothing of its own, because nothing is needed: it holds only what the latest snapshot
//! names, which is this device's picture and one for each paired device. A window left open for a month
//! while devices come and go lets each one go as soon as no snapshot names it.

use peersus_core::profile::{Digest, Picture};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Default)]
pub struct Shelf {
    held: BTreeMap<Digest, Arc<Picture>>,
    /// Asked for and not yet answered, so that a snapshot arriving meanwhile does not ask again.
    waiting: BTreeSet<Digest>,
}

impl Shelf {
    /// The picture for `digest`, if the window holds it. Shared, not copied: a picture is up to sixty
    /// kilobytes of pixels, and the window reads it every time it draws.
    pub fn get(&self, digest: &Digest) -> Option<Arc<Picture>> {
        self.held.get(digest).cloned()
    }

    /// The digests to ask the owner for, given what the latest snapshot names. Each is asked for once:
    /// it is marked as waited-for here, and not offered again until an answer or a failure comes.
    pub fn to_ask(&mut self, named: impl IntoIterator<Item = Digest>) -> Vec<Digest> {
        let mut asked = Vec::new();
        for digest in named {
            if !self.held.contains_key(&digest) && self.waiting.insert(digest) {
                asked.push(digest);
            }
        }
        asked
    }

    /// An answer arrived: keep the picture, but only if it is still awaited and is the picture that was
    /// asked for. A picture the snapshots stopped naming while it was on its way is no longer awaited
    /// (`trim` let go of the wait), and is not let back in; an owner that answered with another picture
    /// is not believed, and the digest is left to be asked again.
    pub fn arrived(&mut self, digest: Digest, picture: Picture) -> bool {
        if !self.waiting.remove(&digest) || picture.digest() != digest {
            return false;
        }
        self.held.insert(digest, Arc::new(picture));
        true
    }

    /// The owner would not give it (or did not answer): it is no longer waited for, so a later snapshot
    /// that still names it can ask once more.
    pub fn failed(&mut self, digest: &Digest) {
        self.waiting.remove(digest);
    }

    /// Let go of what the snapshot no longer names, and of any wait for it. What is still named is kept.
    pub fn trim(&mut self, named: &BTreeSet<Digest>) {
        self.held.retain(|digest, _| named.contains(digest));
        self.waiting.retain(|digest| named.contains(digest));
    }
}

/// What the shelf holds, as the window keeps it in its state: a map of digest to shared picture. Two are
/// equal when they hold the same digests, which is when they hold the same pictures, so the window
/// redraws when a picture arrives or goes and not when nothing changed.
#[derive(Clone, Default)]
pub struct Held(pub std::sync::Arc<BTreeMap<Digest, Arc<Picture>>>);

impl PartialEq for Held {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.keys().eq(other.0.keys())
    }
}

impl Held {
    pub fn get(&self, digest: &Digest) -> Option<Arc<Picture>> {
        self.0.get(digest).cloned()
    }
}

impl Shelf {
    /// What is held now, shared and not copied.
    pub fn held(&self) -> Held {
        Held(Arc::new(self.held.clone()))
    }
}

/// Every picture a snapshot names: the own one and each peer's, once each.
pub fn named_by(view: &crate::view::View) -> BTreeSet<Digest> {
    view.profile_picture
        .into_iter()
        .chain(view.devices.iter().filter_map(|device| device.picture))
        .collect()
}

impl Shelf {
    /// What a new snapshot asks of the window: let go of what it no longer names, and return what has to
    /// be fetched. The one step the window takes on every snapshot, so that the order (let go first, so
    /// a picture dropped and named again is asked for again) is decided here and not in the task.
    pub fn follow(&mut self, named: &BTreeSet<Digest>) -> Vec<Digest> {
        self.trim(named);
        self.to_ask(named.iter().copied())
    }
}

/// The shelf as the window runs it, behind a shared handle: what every snapshot does, whether or not the
/// view changed. This is a separate step from the view's own "did anything change" check on purpose: the
/// view starts from the snapshot the window opened on, so its first pass changes nothing and would never
/// fetch a picture already saved, and a fetch that failed would never be tried again on a quiet owner.
#[derive(Clone, Default)]
pub struct Driven(std::rc::Rc<std::cell::RefCell<Shelf>>);

impl Driven {
    /// A snapshot has arrived naming `named`. Fetch, through `fetch`, each picture not held and not
    /// already awaited, and call `publish` with what is held whenever that changes (now, for what was let
    /// go of, and again as each answer comes). `fetch` returns `None` when the owner would not give it.
    ///
    /// Awaiting the answers is left to the caller's executor: this returns the futures to run, each of
    /// which finishes by updating the shelf and publishing.
    pub fn follow<F, Fut, P>(
        &self,
        named: &BTreeSet<Digest>,
        fetch: F,
        publish: P,
    ) -> Vec<impl std::future::Future<Output = ()> + use<F, Fut, P>>
    where
        F: Fn(Digest) -> Fut + Clone + 'static,
        Fut: std::future::Future<Output = Option<Picture>> + 'static,
        P: Fn(Held) + Clone + 'static,
    {
        let asked = self.0.borrow_mut().follow(named);
        publish(self.0.borrow().held());
        asked
            .into_iter()
            .map(|digest| {
                let (shelf, fetch, publish) = (self.0.clone(), fetch.clone(), publish.clone());
                async move {
                    match fetch(digest).await {
                        Some(picture) => {
                            shelf.borrow_mut().arrived(digest, picture);
                        }
                        None => shelf.borrow_mut().failed(&digest),
                    }
                    publish(shelf.borrow().held());
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(seed: u8) -> Picture {
        Picture::new(
            2,
            2,
            (0..16).map(|n| (n as u8).wrapping_add(seed)).collect(),
        )
        .unwrap()
    }

    #[test]
    fn a_held_picture_is_shared_and_not_copied_each_time_it_is_read() {
        let mut shelf = Shelf::default();
        let own = picture(1);
        shelf.to_ask([own.digest()]);
        shelf.arrived(own.digest(), own.clone());
        let (first, second) = (
            shelf.get(&own.digest()).unwrap(),
            shelf.get(&own.digest()).unwrap(),
        );
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "reading a picture made a second copy of its pixels"
        );
        assert_eq!(*first, own);
    }

    #[test]
    fn what_is_named_and_not_held_is_asked_for_once_and_not_again_while_it_is_awaited() {
        let mut shelf = Shelf::default();
        let (a, b) = (picture(1).digest(), picture(2).digest());
        assert_eq!(shelf.to_ask([a, b]), vec![a, b]);
        // The next snapshot, before any answer, names the same two: nothing is asked again.
        assert!(shelf.to_ask([a, b]).is_empty());
        // A third appears: only it is asked for.
        let c = picture(3).digest();
        assert_eq!(shelf.to_ask([a, b, c]), vec![c]);
        // The same digest named twice in one snapshot is still asked once.
        let mut shelf = Shelf::default();
        assert_eq!(shelf.to_ask([a, a, a]), vec![a]);
    }

    #[test]
    fn a_picture_that_has_arrived_is_held_and_never_asked_for_again() {
        let mut shelf = Shelf::default();
        let own = picture(1);
        shelf.to_ask([own.digest()]);
        assert!(shelf.arrived(own.digest(), own.clone()));
        assert_eq!(shelf.get(&own.digest()).as_deref(), Some(&own));
        assert!(
            shelf.to_ask([own.digest()]).is_empty(),
            "a picture the window holds was asked for again"
        );
    }

    #[test]
    fn an_answer_that_is_another_picture_than_the_one_asked_for_is_not_kept() {
        let mut shelf = Shelf::default();
        let (asked, given) = (picture(1), picture(2));
        shelf.to_ask([asked.digest()]);
        assert!(
            !shelf.arrived(asked.digest(), given.clone()),
            "a different picture was kept"
        );
        assert_eq!(shelf.get(&asked.digest()), None);
        assert_eq!(
            shelf.get(&given.digest()),
            None,
            "a picture nobody asked for was kept under its own digest"
        );
        // And the digest can be asked for again, since nothing was gained.
        assert_eq!(shelf.to_ask([asked.digest()]), vec![asked.digest()]);
    }

    #[test]
    fn a_refusal_lets_a_later_snapshot_ask_once_more() {
        let mut shelf = Shelf::default();
        let digest = picture(1).digest();
        shelf.to_ask([digest]);
        assert!(shelf.to_ask([digest]).is_empty());
        shelf.failed(&digest);
        assert_eq!(shelf.to_ask([digest]), vec![digest]);
    }

    #[test]
    fn what_no_snapshot_names_any_more_is_let_go_and_what_is_named_is_kept() {
        let mut shelf = Shelf::default();
        let (a, b) = (picture(1), picture(2));
        for p in [&a, &b] {
            shelf.to_ask([p.digest()]);
            shelf.arrived(p.digest(), p.clone());
        }
        shelf.trim(&BTreeSet::from([a.digest()]));
        assert!(
            shelf.get(&a.digest()).is_some(),
            "a picture still named was let go"
        );
        assert!(
            shelf.get(&b.digest()).is_none(),
            "a picture no longer named was kept"
        );
        // Something awaited that is no longer named is not awaited any more either, so naming it again asks.
        let c = picture(3).digest();
        shelf.to_ask([c]);
        shelf.trim(&BTreeSet::new());
        assert_eq!(shelf.to_ask([c]), vec![c]);
    }

    #[test]
    fn the_held_state_changes_when_a_picture_comes_or_goes_and_not_when_nothing_did() {
        let mut shelf = Shelf::default();
        let before = shelf.held();
        assert!(
            before == shelf.held(),
            "an unchanged shelf looked changed, so the window would redraw for nothing"
        );
        let a = picture(1);
        shelf.to_ask([a.digest()]);
        shelf.arrived(a.digest(), a.clone());
        let with = shelf.held();
        assert!(
            before != with,
            "a picture arrived and the window would not redraw"
        );
        assert_eq!(with.get(&a.digest()).as_deref(), Some(&a));
        assert!(with == shelf.held());
        shelf.trim(&BTreeSet::new());
        assert!(
            with != shelf.held(),
            "a picture went and the window would not redraw"
        );
    }

    #[test]
    fn an_answer_for_a_picture_no_snapshot_names_any_more_is_not_let_back_in() {
        let mut shelf = Shelf::default();
        let (stays, goes) = (picture(1), picture(2));
        let both: BTreeSet<_> = [stays.digest(), goes.digest()].into_iter().collect();
        assert_eq!(shelf.follow(&both).len(), 2);
        // A snapshot stops naming one of them while its answer is still on the way.
        let only: BTreeSet<_> = [stays.digest()].into_iter().collect();
        assert!(shelf.follow(&only).is_empty());
        // The late answer is refused, and the one still named is taken as ever.
        assert!(
            !shelf.arrived(goes.digest(), goes.clone()),
            "a picture nobody names was let back in"
        );
        assert!(shelf.get(&goes.digest()).is_none());
        assert!(shelf.arrived(stays.digest(), stays.clone()));
        assert!(shelf.get(&stays.digest()).is_some());
        // Named again later, it is asked for again and then taken.
        assert_eq!(shelf.follow(&both), vec![goes.digest()]);
        assert!(shelf.arrived(goes.digest(), goes));
    }

    /// Run futures to completion without an executor: the ones here never wait on anything outside.
    fn run_all<T>(futures: Vec<impl std::future::Future<Output = T>>) -> Vec<T> {
        use std::{
            pin::pin,
            task::{Context, Poll, Waker},
        };
        let mut context = Context::from_waker(Waker::noop());
        futures
            .into_iter()
            .map(|future| match pin!(future).poll(&mut context) {
                Poll::Ready(value) => value,
                Poll::Pending => panic!("a fetch that needs nothing outside did not finish"),
            })
            .collect()
    }

    #[test]
    fn a_picture_saved_before_the_window_opened_is_fetched_and_a_failed_fetch_is_tried_again_on_the_same_snapshot()
     {
        use std::{
            cell::{Cell, RefCell},
            rc::Rc,
        };
        let saved = picture(7);
        let named: BTreeSet<_> = [saved.digest()].into_iter().collect();
        let driven = Driven::default();
        let published: Rc<RefCell<Vec<Held>>> = Rc::default();
        let publish = {
            let published = published.clone();
            move |held: Held| published.borrow_mut().push(held)
        };
        // The owner fails the first time it is asked and answers the second.
        let asks = Rc::new(Cell::new(0));
        let fetch = {
            let (asks, saved) = (asks.clone(), saved.clone());
            move |_digest: Digest| {
                let (asks, saved) = (asks.clone(), saved.clone());
                async move {
                    asks.set(asks.get() + 1);
                    (asks.get() > 1).then_some(saved)
                }
            }
        };

        // The first snapshot: nothing has changed in the view, and the picture is still asked for.
        run_all(driven.follow(&named, fetch.clone(), publish.clone()));
        assert_eq!(
            asks.get(),
            1,
            "a picture saved before the window opened was never asked for"
        );
        assert!(
            published
                .borrow()
                .last()
                .unwrap()
                .get(&saved.digest())
                .is_none(),
            "a refused fetch was taken for a picture"
        );

        // The same snapshot again: the owner is asked once more, and this time it gives the picture.
        run_all(driven.follow(&named, fetch.clone(), publish.clone()));
        assert_eq!(
            asks.get(),
            2,
            "a fetch that failed was not tried again on an unchanged snapshot"
        );
        assert_eq!(
            published
                .borrow()
                .last()
                .unwrap()
                .get(&saved.digest())
                .as_deref(),
            Some(&saved)
        );

        // And once held, the same snapshot again asks for nothing.
        run_all(driven.follow(&named, fetch, publish));
        assert_eq!(asks.get(), 2, "a picture already held was asked for again");
    }

    #[test]
    fn a_picture_a_snapshot_stops_naming_leaves_what_the_window_holds_at_once_with_nothing_to_fetch()
     {
        use std::{cell::RefCell, rc::Rc};
        let kept = picture(3);
        let named: BTreeSet<_> = [kept.digest()].into_iter().collect();
        let driven = Driven::default();
        let published: Rc<RefCell<Vec<Held>>> = Rc::default();
        let publish = {
            let published = published.clone();
            move |held: Held| published.borrow_mut().push(held)
        };
        let give = {
            let kept = kept.clone();
            move |_digest: Digest| {
                let kept = kept.clone();
                async move { Some(kept) }
            }
        };
        run_all(driven.follow(&named, give.clone(), publish.clone()));
        assert!(
            published
                .borrow()
                .last()
                .unwrap()
                .get(&kept.digest())
                .is_some()
        );
        // The next snapshot names nothing: there is nothing to fetch, and what is held must still go.
        let nothing = BTreeSet::new();
        let to_run = driven.follow(&nothing, give, publish);
        assert!(to_run.is_empty());
        assert!(
            published
                .borrow()
                .last()
                .unwrap()
                .get(&kept.digest())
                .is_none(),
            "a picture no snapshot names stayed in what the window draws from"
        );
    }
}
