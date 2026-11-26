//! The extension HTTP store is read without unlocking the Secret Service.
use dbus_secret_service::{Collection, EncryptionType, Error, SecretService};
use keyring::secret_service::SsCredential;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

const UNAVAILABLE: &str = "The encrypted credential store is unavailable";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const LOOKUP_WORKERS: usize = 2;

pub(super) async fn lookup(
    workers: Arc<Semaphore>,
    read: impl FnOnce(Instant) -> Result<bool, &'static str> + Send + 'static,
) -> Result<bool, &'static str> {
    let deadline = tokio::time::Instant::now() + LOOKUP_TIMEOUT;
    let permit = workers.try_acquire_owned().map_err(|_| UNAVAILABLE)?;
    let worker = tokio::task::spawn_blocking(move || {
        // A reply timing out or a caller cancelling doesn't stop a running D-Bus call.
        let _permit = permit;
        if tokio::time::Instant::now() >= deadline {
            return Err(UNAVAILABLE);
        }
        read(deadline.into_std())
    });
    tokio::time::timeout_at(deadline, worker)
        .await
        .map_err(|_| UNAVAILABLE)?
        .map_err(|_| UNAVAILABLE)?
}

pub(super) fn present(entry: &keyring::Entry, deadline: Instant) -> Result<bool, &'static str> {
    let credential = entry
        .get_credential()
        .downcast_ref::<SsCredential>()
        .ok_or(UNAVAILABLE)?;
    let attribute = |name| {
        credential
            .attributes
            .get(name)
            .map(String::as_str)
            .ok_or(UNAVAILABLE)
    };
    let legacy = [
        ("service", attribute("service")?),
        ("username", attribute("username")?),
    ];
    let target = attribute("target")?;
    let scoped: HashMap<_, _> = legacy.into_iter().chain([("target", target)]).collect();
    let within_deadline = || {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(Error::Unavailable)
    };
    within_deadline().map_err(|_| UNAVAILABLE)?;
    // The pinned Secret Service client also sets a two-second timeout for every D-Bus method.
    let service = SecretService::connect(EncryptionType::Dh).map_err(|_| UNAVAILABLE)?;
    within_deadline().map_err(|_| UNAVAILABLE)?;
    let collections = service.get_all_collections().map_err(|_| UNAVAILABLE)?;
    let search = |collection: &Collection<'_>, attributes| {
        search_unlocked(
            || {
                within_deadline()?;
                collection.is_locked()
            },
            || {
                within_deadline()?;
                collection
                    .search_items(attributes)
                    .map(|found| !found.is_empty())
            },
        )
        .map_err(|_| UNAVAILABLE)
    };
    // KeePassXC can hide locked collections and even prompt from service-wide SearchItems.
    // Inspect the collections themselves, without requesting item contents or unlocking.
    let scoped = collections
        .iter()
        .map(|collection| search(collection, scoped.clone()));
    let legacy = std::iter::once_with(|| {
        if target != "default" {
            return Ok(false);
        }
        // The existing keyring backend falls back to legacy entries in the default collection only.
        within_deadline().map_err(|_| UNAVAILABLE)?;
        in_default(service.get_default_collection(), |collection| {
            search(&collection, HashMap::from(legacy))
        })
    });
    any_present(scoped.chain(legacy))
}

fn any_present(
    results: impl IntoIterator<Item = Result<bool, &'static str>>,
) -> Result<bool, &'static str> {
    let mut absent = Ok(false);
    for result in results {
        match result {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => absent = Err(error),
        }
    }
    absent
}

/// Refuse a collection found locked before or after search. The probes are not atomic
/// with the query and cannot detect a lock and unlock that both happen during it.
fn search_unlocked<T>(
    locked: impl Fn() -> Result<bool, Error>,
    search: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    if locked()? {
        return Err(Error::Locked);
    }
    let found = search()?;
    // A provider can return an empty list if it locked while processing the query.
    if locked()? {
        return Err(Error::Locked);
    }
    Ok(found)
}

fn in_default<C>(
    alias: Result<C, Error>,
    read: impl FnOnce(C) -> Result<bool, &'static str>,
) -> Result<bool, &'static str> {
    match alias {
        Ok(collection) => read(collection),
        // dbus-secret-service returns NoResult specifically when ReadAlias returned '/'.
        Err(Error::NoResult) => Ok(false),
        Err(_) => Err(UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn confirmed_presence_wins_over_inconclusive_collections_in_either_order() {
        for results in [[Err(UNAVAILABLE), Ok(true)], [Ok(true), Err(UNAVAILABLE)]] {
            let legacy = std::iter::once_with(|| panic!("scoped match should skip the alias"));
            assert_eq!(any_present(results.into_iter().chain(legacy)), Ok(true));
        }
        let legacy = in_default(Ok(()), |_| Ok(true));
        assert_eq!(any_present([Err(UNAVAILABLE), legacy]), Ok(true));
    }

    #[test]
    fn absence_requires_conclusive_scoped_and_default_lookups() {
        let missing = in_default(Err::<(), _>(Error::NoResult), |_| panic!("no collection"));
        assert_eq!(any_present([Err(UNAVAILABLE), missing]), Err(UNAVAILABLE));
        assert_eq!(any_present([Ok(false), Err(UNAVAILABLE)]), Err(UNAVAILABLE));
        assert_eq!(any_present([Ok(false), missing]), Ok(false));
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_and_cancelled_lookups_keep_their_permits_until_the_workers_exit() {
        let workers = Arc::new(Semaphore::new(LOOKUP_WORKERS));
        let start_worker = || {
            let (started, running) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let task = tokio::spawn(lookup(workers.clone(), move |_| {
                started.send(()).map_err(|_| UNAVAILABLE)?;
                blocked.recv().map_err(|_| UNAVAILABLE)?;
                Ok(true)
            }));
            (task, running, release)
        };
        let (first, first_running, release_first) = start_worker();
        let (second, second_running, release_second) = start_worker();
        first_running.await.unwrap();
        second_running.await.unwrap();

        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        tokio::time::advance(LOOKUP_TIMEOUT + Duration::from_millis(1)).await;
        assert_eq!(first.await.unwrap(), Err(UNAVAILABLE));
        assert_eq!(
            lookup(workers.clone(), |_| panic!("both workers still running")).await,
            Err(UNAVAILABLE)
        );

        release_first.send(()).unwrap();
        release_second.send(()).unwrap();
        // Wait until both workers exited without racing their final permit drops.
        drop(
            workers
                .clone()
                .acquire_many_owned(LOOKUP_WORKERS as u32)
                .await
                .unwrap(),
        );
        assert_eq!(lookup(workers, |_| Ok(true)).await, Ok(true));
    }

    #[test]
    fn a_locked_or_relocked_collection_cannot_report_empty() {
        for starts_locked in [true, false] {
            let locked = Cell::new(starts_locked);
            let searched = Cell::new(false);
            let found = search_unlocked(
                || Ok(locked.get()),
                || {
                    searched.set(true);
                    locked.set(true);
                    Ok(Vec::<()>::new())
                },
            );
            assert!(matches!(found, Err(Error::Locked)));
            assert_eq!(searched.get(), !starts_locked);
        }
    }

    #[test]
    fn only_an_explicitly_missing_default_alias_is_an_empty_legacy_fallback() {
        assert_eq!(
            in_default(Err::<(), _>(Error::NoResult), |_| panic!("no collection")),
            Ok(false)
        );
        for error in [Error::Locked, Error::Unavailable, Error::Parse] {
            assert_eq!(
                in_default(Err::<(), _>(error), |_| panic!("no collection")),
                Err(UNAVAILABLE)
            );
        }
        assert_eq!(in_default(Ok(()), |_| Err(UNAVAILABLE)), Err(UNAVAILABLE));
        assert_eq!(in_default(Ok(()), |_| Ok(true)), Ok(true));
        assert!(matches!(
            search_unlocked(|| Ok(false), || Err::<(), _>(Error::NoResult)),
            Err(Error::NoResult)
        ));
    }
}
