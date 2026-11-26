//! Linux Secret Service unlock prompts. No secrets, passwords or credential writes.
use std::time::Duration;
use zbus::OwnedObjectPath;
#[cfg(any(not(feature = "test-identity"), test))]
use zbus::OwnedVariant;

#[cfg(any(not(feature = "test-identity"), test))]
pub(super) const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// The provider has the keyring and was asked to unlock it, but refused to show the password prompt: it found no prompter it could
/// reach for this program (a provider can decide that a program is not in a graphical session and look for a terminal prompter that
/// is not there). The keyring itself is reachable, so this must not read as if it were not.
#[cfg(any(not(feature = "test-identity"), test))]
const PROMPT_NOT_SHOWN: &str = "The system keyring is locked and its service did not show the password prompt; existing identity and pairings were not changed";

/// How long one probe waits for the bus. A probe never shows anything, so a slow answer is "not available yet".
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether the keyring can be read now, asked without showing anything, unlocking anything or starting its service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// The service answers and every collection is unlocked: the identity can be read or made without a prompt.
    Ready,
    /// The service answers but a collection is locked. Only the person's own unlock (at sign-in) opens it.
    Locked,
    /// Nothing owns the keyring's name, or it cannot be reached. Nothing was started and nothing was asked of it.
    Unavailable,
}

/// Probe only a provider that is already on the bus, without unlocking or prompting.
pub(super) async fn state_async() -> State {
    let connection = match zbus::connection::Builder::session()
        .method_timeout(PROBE_TIMEOUT)
        .build()
        .await
    {
        Ok(connection) => connection,
        Err(_) => return State::Unavailable,
    };
    state_on(&connection, "org.freedesktop.secrets").await
}

async fn state_on(connection: &zbus::Connection, destination: &str) -> State {
    // The bus's own name, never the keyring's: asking this starts nothing.
    let bus = match zbus::Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    {
        Ok(bus) => bus,
        Err(_) => return State::Unavailable,
    };
    let owned: bool = match bus.call("NameHasOwner", &(destination,)).await {
        Ok(owned) => owned,
        Err(_) => return State::Unavailable,
    };
    if !owned {
        return State::Unavailable;
    }
    let service = match zbus::Proxy::new(
        connection,
        destination,
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    )
    .await
    {
        Ok(service) => service,
        Err(_) => return State::Unavailable,
    };
    // Attribute-only lookup: no secret is read and no prompt is shown.
    let attributes = std::collections::HashMap::from([
        ("service", "Peersus"),
        ("username", "transport-identity"),
    ]);
    let (_, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
        match service.call("SearchItems", &(attributes,)).await {
            Ok(found) => found,
            Err(_) => return State::Unavailable,
        };
    let collections: Vec<OwnedObjectPath> = match service.get_property("Collections").await {
        Ok(collections) => collections,
        Err(_) => return State::Unavailable,
    };
    let mut locked_somewhere = !locked.is_empty();
    for path in &collections {
        let collection = match zbus::Proxy::new(
            connection,
            destination,
            path.as_str(),
            "org.freedesktop.Secret.Collection",
        )
        .await
        {
            Ok(collection) => collection,
            Err(_) => return State::Unavailable,
        };
        match collection.get_property::<bool>("Locked").await {
            Ok(true) => locked_somewhere = true,
            Ok(false) => {}
            Err(_) => return State::Unavailable,
        }
    }
    if locked_somewhere {
        State::Locked
    } else {
        State::Ready
    }
}

// Synthetic identities never ask the provider to open the keyring.
#[cfg(not(feature = "test-identity"))]
pub(super) fn prepare() -> Result<(), &'static str> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start system keyring access; existing identity was not changed")?;
    runtime.block_on(async {
        let connection = zbus::connection::Builder::session()
            // The password dialog is shown while Unlock/Prompt are awaited: a few seconds was the time a person takes to find the dialog, and
            // the call failed as an inaccessible keyring before the password could be typed.
            .method_timeout(PROMPT_TIMEOUT)
            .build()
            .await
            .map_err(|_| "System keyring is unavailable; existing identity and pairings were not changed")?;
        prepare_on(&connection, "org.freedesktop.secrets", PROMPT_TIMEOUT).await
    })
}

#[cfg(any(not(feature = "test-identity"), test))]
async fn prepare_on(
    connection: &zbus::Connection,
    destination: &str,
    timeout: Duration,
) -> Result<(), &'static str> {
    let access_failed =
        "Cannot access system keyring; existing identity and pairings were not changed";
    let service = zbus::Proxy::new(
        connection,
        destination,
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    )
    .await
    .map_err(|_| access_failed)?;
    // Attribute-only lookup: never read a secret to decide whether an unlock is needed.
    let attributes = std::collections::HashMap::from([
        ("service", "Peersus"),
        ("username", "transport-identity"),
    ]);
    let (_unlocked, mut objects): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
        .call("SearchItems", &(attributes,))
        .await
        .map_err(|_| access_failed)?;
    // A visible match cannot rule out an older identity hidden by this provider.
    // This path is used only for already-paired profiles, never generic keyring callers.
    let collections: Vec<OwnedObjectPath> = service
        .get_property("Collections")
        .await
        .map_err(|_| access_failed)?;
    for path in &collections {
        let collection = zbus::Proxy::new(
            connection,
            destination,
            path.as_str(),
            "org.freedesktop.Secret.Collection",
        )
        .await
        .map_err(|_| access_failed)?;
        if collection
            .get_property::<bool>("Locked")
            .await
            .map_err(|_| access_failed)?
        {
            objects.push(path.clone());
        }
    }
    objects.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    objects.dedup();
    if objects.is_empty() {
        return Ok(());
    }
    // A provider may choose how it asks for the password by the Secret Service session this connection opened: that is where it
    // learns who the caller is and whether it runs in a graphical session. A connection that asks to unlock without one can be
    // taken for a terminal program and given a prompter that is not there. No secret crosses this session; it ends with the
    // connection.
    let _session: (OwnedVariant, OwnedObjectPath) = service
        .call("OpenSession", &("plain", zbus::Variant::from("")))
        .await
        .map_err(|_| access_failed)?;
    let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
        .call("Unlock", &(objects,))
        .await
        .map_err(|_| access_failed)?;
    if prompt.as_str() == "/" {
        return verify_unlocked(connection, destination, &collections).await;
    }
    let prompt = zbus::Proxy::new(
        connection,
        destination,
        prompt.as_str(),
        "org.freedesktop.Secret.Prompt",
    )
    .await
    .map_err(|_| access_failed)?;
    // Subscribe first: a provider may complete synchronously inside Prompt.
    let mut completed = prompt
        .receive_signal("Completed")
        .await
        .map_err(|_| access_failed)?;
    // A provider that fails here never showed anything to the person: it is not a cancel and not an unreachable keyring.
    prompt
        .call::<_, _, ()>("Prompt", &("",))
        .await
        .map_err(|_| PROMPT_NOT_SHOWN)?;
    use futures_lite::StreamExt;
    let completed = tokio::time::timeout(timeout, completed.next()).await;
    let message = match completed {
        Ok(Some(message)) => message,
        Ok(None) => return Err(access_failed),
        Err(_) => {
            let _ = prompt.call::<_, _, ()>("Dismiss", &()).await;
            return Err(
                "System keyring unlock timed out; existing identity and pairings were not changed",
            );
        }
    };
    let (dismissed, _result): (bool, OwnedVariant) =
        message.body().deserialize().map_err(|_| access_failed)?;
    if dismissed {
        return Err(
            "System keyring unlock was cancelled; existing identity and pairings were not changed",
        );
    }
    verify_unlocked(connection, destination, &collections).await
}

#[cfg(any(not(feature = "test-identity"), test))]
async fn verify_unlocked(
    connection: &zbus::Connection,
    destination: &str,
    collections: &[OwnedObjectPath],
) -> Result<(), &'static str> {
    for path in collections {
        let collection = zbus::Proxy::new(
            connection,
            destination,
            path.as_str(),
            "org.freedesktop.Secret.Collection",
        )
        .await
        .map_err(|_| "Cannot verify system keyring unlock; existing identity was not changed")?;
        if collection
            .get_property::<bool>("Locked")
            .await
            .map_err(|_| "Cannot verify system keyring unlock; existing identity was not changed")?
        {
            return Err(
                "System keyring is still locked; existing identity and pairings were not changed",
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        locked: bool,
        hidden: bool,
        dismissed: bool,
        visible_duplicate: bool,
        silent: bool,
        refuse_unlock: bool,
        /// The provider refuses to show a prompt at all (it has no prompter it can reach for this caller).
        no_prompter: bool,
        /// The provider picks its prompter by the session the caller opened, and has only a terminal one for a caller without.
        prompter_by_session: bool,
        /// The bus names that opened a session.
        sessions: Vec<String>,
        prompts: usize,
        unlocks: usize,
        dismissals: usize,
    }
    struct Service(Arc<Mutex<State>>);
    struct Collection(Arc<Mutex<State>>);
    struct Prompt(Arc<Mutex<State>>);
    const COLLECTION: &str = "/org/freedesktop/secrets/collection/test";
    const ITEM: &str = "/org/freedesktop/secrets/collection/test/item";
    const PROMPT: &str = "/org/freedesktop/secrets/prompt/test";

    #[zbus::interface(name = "org.freedesktop.Secret.Service")]
    impl Service {
        fn open_session(
            &self,
            algorithm: &str,
            _input: zbus::Variant<'_>,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> (OwnedVariant, OwnedObjectPath) {
            assert_eq!(algorithm, "plain");
            if let Some(sender) = header.sender() {
                self.0.lock().unwrap().sessions.push(sender.to_string());
            }
            (
                zbus::Variant::from("").try_into_owned().unwrap(),
                "/org/freedesktop/secrets/session/test".try_into().unwrap(),
            )
        }
        fn search_items(
            &self,
            attributes: std::collections::HashMap<String, String>,
        ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
            assert_eq!(
                attributes.get("service").map(String::as_str),
                Some("Peersus")
            );
            assert_eq!(
                attributes.get("username").map(String::as_str),
                Some("transport-identity")
            );
            let state = self.0.lock().unwrap();
            if state.locked {
                (
                    if state.visible_duplicate {
                        vec![ITEM.try_into().unwrap()]
                    } else {
                        vec![]
                    },
                    if state.hidden {
                        vec![]
                    } else {
                        vec![ITEM.try_into().unwrap()]
                    },
                )
            } else {
                (vec![ITEM.try_into().unwrap()], vec![])
            }
        }
        #[zbus(property)]
        fn collections(&self) -> Vec<OwnedObjectPath> {
            vec![COLLECTION.try_into().unwrap()]
        }
        fn unlock(&self, objects: Vec<OwnedObjectPath>) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
            assert!(objects.iter().any(|p| p.as_str() == COLLECTION));
            assert!(
                objects
                    .iter()
                    .all(|p| p.as_str() == COLLECTION || p.as_str() == ITEM)
            );
            let mut state = self.0.lock().unwrap();
            state.unlocks += 1;
            if state.refuse_unlock {
                return (vec![], "/".try_into().unwrap());
            }
            (vec![], PROMPT.try_into().unwrap())
        }
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Collection")]
    impl Collection {
        #[zbus(property)]
        fn locked(&self) -> bool {
            self.0.lock().unwrap().locked
        }
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
    impl Prompt {
        async fn prompt(
            &self,
            window_id: &str,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            assert!(window_id.is_empty());
            let dismissed = {
                let mut state = self.0.lock().unwrap();
                state.prompts += 1;
                let has_session = header
                    .sender()
                    .is_some_and(|sender| state.sessions.iter().any(|s| s == sender.as_str()));
                if state.no_prompter || (state.prompter_by_session && !has_session) {
                    return Err(zbus::fdo::Error::Failed(
                        "CLI prompter failed: Destination does not exist".into(),
                    ));
                }
                if state.silent {
                    return Ok(());
                }
                if !state.dismissed {
                    state.locked = false;
                }
                state.dismissed
            };
            let result = zbus::Variant::from(vec![OwnedObjectPath::try_from(COLLECTION).unwrap()])
                .try_into_owned()
                .unwrap();
            Self::completed(&emitter, dismissed, result)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
        }
        fn dismiss(&self) {
            self.0.lock().unwrap().dismissals += 1;
        }
        #[zbus(signal)]
        async fn completed(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            dismissed: bool,
            result: OwnedVariant,
        ) -> zbus::Result<()>;
    }

    async fn exercise(
        locked: bool,
        hidden: bool,
        dismissed: bool,
    ) -> (Result<(), &'static str>, Arc<Mutex<State>>) {
        exercise_state(State {
            locked,
            hidden,
            dismissed,
            ..State::default()
        })
        .await
    }

    async fn exercise_state(initial: State) -> (Result<(), &'static str>, Arc<Mutex<State>>) {
        exercise_with_timeout(initial, PROMPT_TIMEOUT).await
    }

    async fn exercise_with_timeout(
        initial: State,
        timeout: Duration,
    ) -> (Result<(), &'static str>, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(initial));
        let name = format!(
            "ro.peersus.IdentityTest.p{}n{}",
            std::process::id(),
            rand::random::<u32>()
        );
        let service = zbus::connection::Builder::session()
            .name(name.as_str())
            .serve_at("/org/freedesktop/secrets", Service(state.clone()))
            .serve_at(COLLECTION, Collection(state.clone()))
            .serve_at(PROMPT, Prompt(state.clone()))
            .build()
            .await
            .unwrap();
        let client = zbus::Connection::session().await.unwrap();
        let result = prepare_on(&client, &name, timeout).await;
        drop(service);
        (result, state)
    }

    #[tokio::test]
    async fn hidden_locked_identity_requests_native_prompt_before_retry() {
        let (result, state) = exercise(true, true, false).await;
        assert!(result.is_ok());
        let state = state.lock().unwrap();
        assert_eq!(state.prompts, 1);
        assert_eq!(state.unlocks, 1);
        assert!(!state.locked);
    }
    #[tokio::test]
    async fn reported_locked_identity_also_uses_native_prompt() {
        let (result, state) = exercise(true, false, false).await;
        assert!(result.is_ok());
        assert_eq!(state.lock().unwrap().prompts, 1);
    }
    #[tokio::test]
    async fn unlocked_identity_never_prompts() {
        let (result, state) = exercise(false, false, false).await;
        assert!(result.is_ok());
        assert_eq!(state.lock().unwrap().prompts, 0);
        assert_eq!(state.lock().unwrap().unlocks, 0);
    }
    #[tokio::test]
    async fn cancelled_native_prompt_is_not_reported_as_missing_identity() {
        let (result, state) = exercise(true, true, true).await;
        assert_eq!(
            result,
            Err(
                "System keyring unlock was cancelled; existing identity and pairings were not changed"
            )
        );
        assert!(state.lock().unwrap().locked);
        assert_eq!(state.lock().unwrap().prompts, 1);
    }

    #[tokio::test]
    async fn a_visible_duplicate_does_not_hide_the_locked_original_from_unlock() {
        let (result, state) = exercise_state(State {
            locked: true,
            hidden: true,
            visible_duplicate: true,
            ..State::default()
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(state.lock().unwrap().prompts, 1);
        assert!(!state.lock().unwrap().locked);
    }

    #[tokio::test]
    async fn unanswered_prompt_is_dismissed_at_the_deadline() {
        let (result, state) = exercise_with_timeout(
            State {
                locked: true,
                hidden: true,
                silent: true,
                ..State::default()
            },
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(
            result,
            Err("System keyring unlock timed out; existing identity and pairings were not changed")
        );
        assert_eq!(state.lock().unwrap().dismissals, 1);
        assert!(state.lock().unwrap().locked);
    }

    #[tokio::test]
    async fn an_unlock_without_prompt_must_actually_unlock_the_collection() {
        let (result, state) = exercise_state(State {
            locked: true,
            hidden: true,
            refuse_unlock: true,
            ..State::default()
        })
        .await;
        assert_eq!(
            result,
            Err("System keyring is still locked; existing identity and pairings were not changed")
        );
        assert!(state.lock().unwrap().locked);
    }

    #[tokio::test]
    async fn a_provider_that_cannot_show_its_prompt_is_said_to_be_that_and_not_an_unreachable_keyring()
     {
        let (result, state) = exercise_state(State {
            locked: true,
            hidden: true,
            no_prompter: true,
            ..State::default()
        })
        .await;
        assert_eq!(result, Err(PROMPT_NOT_SHOWN));
        assert!(state.lock().unwrap().locked);
        assert_eq!(state.lock().unwrap().prompts, 1);
    }

    #[tokio::test]
    async fn a_provider_that_picks_its_prompter_by_the_callers_session_is_given_one_before_the_unlock()
     {
        let (result, state) = exercise_state(State {
            locked: true,
            hidden: true,
            prompter_by_session: true,
            ..State::default()
        })
        .await;
        assert_eq!(result, Ok(()));
        let state = state.lock().unwrap();
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.prompts, 1);
        assert!(!state.locked);
    }

    #[tokio::test]
    async fn an_unlocked_identity_opens_no_session() {
        let (result, state) = exercise(false, false, false).await;
        assert!(result.is_ok());
        assert!(state.lock().unwrap().sessions.is_empty());
    }

    async fn probe(initial: State) -> (super::State, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(initial));
        let name = format!(
            "ro.peersus.IdentityProbe.p{}n{}",
            std::process::id(),
            rand::random::<u32>()
        );
        let service = zbus::connection::Builder::session()
            .name(name.as_str())
            .serve_at("/org/freedesktop/secrets", Service(state.clone()))
            .serve_at(COLLECTION, Collection(state.clone()))
            .serve_at(PROMPT, Prompt(state.clone()))
            .build()
            .await
            .unwrap();
        let client = zbus::Connection::session().await.unwrap();
        let found = state_on(&client, &name).await;
        drop(service);
        (found, state)
    }

    #[tokio::test]
    async fn a_locked_keyring_is_seen_locked_without_asking_or_unlocking() {
        let (found, state) = probe(State {
            locked: true,
            hidden: true,
            ..State::default()
        })
        .await;
        assert_eq!(found, super::State::Locked);
        let state = state.lock().unwrap();
        assert_eq!(state.prompts, 0);
        assert_eq!(state.unlocks, 0);
        assert!(state.locked);
    }

    #[tokio::test]
    async fn an_unlocked_keyring_is_seen_ready_without_asking_or_unlocking() {
        let (found, state) = probe(State::default()).await;
        assert_eq!(found, super::State::Ready);
        let state = state.lock().unwrap();
        assert_eq!(state.prompts, 0);
        assert_eq!(state.unlocks, 0);
    }

    #[tokio::test]
    async fn a_keyring_nothing_owns_is_not_started_and_reads_as_unavailable() {
        let name = format!(
            "ro.peersus.IdentityProbe.nobody.p{}n{}",
            std::process::id(),
            rand::random::<u32>()
        );
        let client = zbus::Connection::session().await.unwrap();
        assert_eq!(state_on(&client, &name).await, super::State::Unavailable);
    }
}
