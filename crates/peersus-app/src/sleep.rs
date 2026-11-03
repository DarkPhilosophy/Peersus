//! Whether this computer is going to sleep, from the system's login manager, and the word that goes to the paired devices.
//!
//! On a computer "sleeping" means a real suspend: the network is off and nothing answers until it wakes. A screen that goes dark
//! or a session that is idle is NOT it, because the machine is still connected and still answers, so neither is read here.
//!
//! The word has to leave BEFORE the machine suspends, or the other devices would see a connection that froze and call this one
//! offline instead of asleep. So a delay lock on sleep is held for as long as the owner lives: the login manager then waits for it to
//! be let go of before it suspends, which is done only after the word was sent.
//!
//! What this does NOT promise: that the word arrived. It is handed to the connection and a moment is given for it to leave; the other
//! device is not asked to confirm, so a connection that freezes before it is sent still reads as offline there, as it did before.
//!
//! Linux only, from `org.freedesktop.login1`. No polling, no process started, nothing written. Where the system bus or the
//! login manager is missing (a container, a minimal system) the watcher gives up quietly and the device is simply never marked
//! as sleeping, which is what it was before.

/// Follow the login manager for as long as the owner lives, telling it through `client` when this computer sleeps and wakes.
#[cfg(target_os = "linux")]
pub async fn follow(client: crate::runtime::Client) {
    let mut delay = 1;
    loop {
        let connection = zbus::Connection::system().await;
        if let Ok(connection) = connection {
            let said = |asleep: bool| {
                let client = client.clone();
                async move {
                    let _ = client
                        .command(peersus_core::model::Command::SetAsleep { asleep })
                        .await;
                }
            };
            let _ = follow_on(&connection, "org.freedesktop.login1", said).await;
        }
        // A connection that ends is made again, slowly: a bus that restarts must not end the watching for good.
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

/// A delay lock on sleep: while it is held, the login manager waits before it suspends. Dropping the descriptor lets go of it.
#[cfg(target_os = "linux")]
async fn hold_back(manager: &zbus::Proxy<'_>) -> zbus::Result<zbus::zvariant::OwnedFd> {
    manager
        .call(
            "Inhibit",
            &(
                "sleep",
                "Peersus",
                "Telling the paired devices that this one is going to sleep",
                "delay",
            ),
        )
        .await
}

/// Watch `destination`'s login manager and call `say` with `true` when the machine is about to sleep and with `false` when it woke.
/// `say` is awaited before the lock is let go of, so the word is handed over before the machine may suspend.
#[cfg(target_os = "linux")]
async fn follow_on<F, Fut>(
    connection: &zbus::Connection,
    destination: &str,
    say: F,
) -> zbus::Result<()>
where
    F: Fn(bool) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures_lite::StreamExt;
    let manager = zbus::Proxy::new(
        connection,
        destination,
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    // Subscribe before reading: a sleep that begins while this starts must not be lost.
    let mut signals = manager.receive_signal("PrepareForSleep").await?;
    let mut lid_changes = manager.receive_property_changed::<bool>("LidClosed").await;
    let mut owners = manager.receive_owner_changed().await?;
    let mut lock = hold_back(&manager).await.ok();
    // NOTE(@DarkPhilosophy): a closed lid is counted as sleeping, on purpose. A laptop with its lid closed has nobody in front of it, so
    // nobody can accept or read a file, even when it is docked and does not suspend. Marked here so that this choice can be reviewed
    // later; to drop it, make `lid` always false below and the word follows the suspend alone.
    let mut suspending = manager
        .get_property::<bool>("PreparingForSleep")
        .await
        .unwrap_or(false);
    let mut lid = manager
        .get_property::<bool>("LidClosed")
        .await
        .unwrap_or(false);
    let mut said = suspending || lid;
    say(said).await;
    loop {
        tokio::select! {
            signal = signals.next() => {
                let Some(signal) = signal else { return Ok(()) };
                suspending = signal.body().deserialize()?;
            }
            change = lid_changes.next() => {
                let Some(change) = change else { return Ok(()) };
                lid = change.get().await.unwrap_or(lid);
            }
            _ = owners.next() => return Ok(()),
        }
        let now = suspending || lid;
        if now != said {
            // Told first, and only then is the machine let go to sleep: dropping the lock is what lets it.
            say(now).await;
            said = now;
        }
        if suspending {
            lock = None;
        } else if lock.is_none() {
            // Awake again: held back once more for the next sleep.
            lock = hold_back(&manager).await.ok();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// What `Inhibit` was asked, in order: what, who, why, mode.
    type Asked = Arc<Mutex<Vec<(String, String, String, String)>>>;

    /// A login manager that offers only what is watched: the lock, the states, and the signal, so that what is sent and in what
    /// order is seen without a real system bus. What `Inhibit` was asked is kept, to be compared with the interface as documented.
    struct Manager {
        asked: Asked,
        locks: Arc<Mutex<usize>>,
        lid: Arc<Mutex<bool>>,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl Manager {
        fn inhibit(
            &self,
            what: String,
            who: String,
            why: String,
            mode: String,
        ) -> zbus::zvariant::OwnedFd {
            self.asked.lock().unwrap().push((what, who, why, mode));
            *self.locks.lock().unwrap() += 1;
            std::os::fd::OwnedFd::from(std::fs::File::open("/dev/null").unwrap()).into()
        }
        #[zbus(property)]
        fn preparing_for_sleep(&self) -> bool {
            false
        }
        #[zbus(property)]
        fn lid_closed(&self) -> bool {
            *self.lid.lock().unwrap()
        }
        #[zbus(signal)]
        async fn prepare_for_sleep(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            start: bool,
        ) -> zbus::Result<()>;
    }

    /// The fake on a private bus and the watcher on it, with what the watcher said and what it asked kept for the tests.
    struct Rig {
        service: zbus::Connection,
        said: Arc<Mutex<Vec<bool>>>,
        asked: Asked,
        locks: Arc<Mutex<usize>>,
        lid: Arc<Mutex<bool>>,
        worker: tokio::task::JoinHandle<zbus::Result<()>>,
    }

    async fn rig(lid_closed_at_start: bool) -> Rig {
        let (asked, locks, lid) = (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(lid_closed_at_start)),
        );
        let name = format!(
            "ro.peersus.SleepTest.p{}n{}",
            std::process::id(),
            rand::random::<u32>()
        );
        let service = zbus::connection::Builder::session()
            .unwrap()
            .name(name.as_str())
            .unwrap()
            .serve_at(
                "/org/freedesktop/login1",
                Manager {
                    asked: asked.clone(),
                    locks: locks.clone(),
                    lid: lid.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::Connection::session().await.unwrap();
        let said = Arc::new(Mutex::new(Vec::new()));
        let record = said.clone();
        let worker = tokio::spawn(async move {
            follow_on(&client, &name, move |asleep| {
                let record = record.clone();
                async move {
                    record.lock().unwrap().push(asleep);
                }
            })
            .await
        });
        Rig {
            service,
            said,
            asked,
            locks,
            lid,
            worker,
        }
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }

    impl Rig {
        fn said(&self) -> Vec<bool> {
            self.said.lock().unwrap().clone()
        }
        async fn suspend(&self, start: bool) {
            let emitter =
                zbus::object_server::SignalEmitter::new(&self.service, "/org/freedesktop/login1")
                    .unwrap();
            Manager::prepare_for_sleep(&emitter, start).await.unwrap();
        }
        async fn lid(&self, closed: bool) {
            *self.lid.lock().unwrap() = closed;
            let iface = self
                .service
                .object_server()
                .interface::<_, Manager>("/org/freedesktop/login1")
                .await
                .unwrap();
            iface
                .get()
                .await
                .lid_closed_changed(iface.signal_emitter())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn the_lock_is_taken_as_documented_and_the_word_goes_before_the_machine_is_let_go() {
        let rig = rig(false).await;
        until("the lock was never asked for", || {
            *rig.locks.lock().unwrap() > 0
        })
        .await;
        assert_eq!(rig.asked.lock().unwrap()[0].0, "sleep");
        assert_eq!(rig.asked.lock().unwrap()[0].3, "delay");
        // The state read at start comes first (awake: the fake is neither suspending nor closed).
        until("the state at start was not said", || !rig.said().is_empty()).await;
        assert_eq!(rig.said(), vec![false]);
        // It is about to sleep: the word is `true`.
        rig.suspend(true).await;
        until("the sleep was not said", || rig.said().len() >= 2).await;
        assert_eq!(rig.said(), vec![false, true]);
        // And it woke: the word is `false`, and the lock is taken again for the next sleep.
        rig.suspend(false).await;
        until(
            "the wake was not said, or the lock was not taken again",
            || rig.said().len() >= 3 && *rig.locks.lock().unwrap() >= 2,
        )
        .await;
        assert_eq!(rig.said(), vec![false, true, false]);
        rig.worker.abort();
    }

    #[tokio::test]
    async fn a_closed_lid_is_sleeping_even_when_the_machine_does_not_suspend_and_a_wake_with_the_lid_shut_stays_so()
     {
        // Started with the lid already closed: asleep from the first word.
        let rig = rig(true).await;
        until("the state at start was not said", || !rig.said().is_empty()).await;
        assert_eq!(rig.said(), vec![true]);
        // Opened: awake, with no suspend at all.
        rig.lid(false).await;
        until("the opened lid was not said", || rig.said().len() >= 2).await;
        assert_eq!(rig.said(), vec![true, false]);
        // Closed again, and then the machine suspends and wakes WITH THE LID STILL SHUT: it is never reported awake in between.
        rig.lid(true).await;
        until("the closed lid was not said", || rig.said().len() >= 3).await;
        rig.suspend(true).await;
        rig.suspend(false).await;
        until("the lock was not taken again after the wake", || {
            *rig.locks.lock().unwrap() >= 2
        })
        .await;
        assert_eq!(
            rig.said(),
            vec![true, false, true],
            "a wake with the lid shut must not say awake: {:?}",
            rig.said()
        );
        rig.worker.abort();
    }
}
