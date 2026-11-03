//! A platform launcher owns an actual external lifecycle rather than a pretend executable.
#![cfg(unix)]
use peersus_plugin_api::{hub::Hub, manifest::Manifest};
use peersus_plugin_host::{
    Event, Plugin, Service,
    supervisor::{Bootstrap, Launcher, Lifecycle, Policy},
};
use std::{
    collections::BTreeSet,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

struct Counts {
    launches: AtomicUsize,
    stops: AtomicUsize,
    drops: AtomicUsize,
    launched: Semaphore,
}
impl Default for Counts {
    fn default() -> Self {
        Self {
            launches: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
            launched: Semaphore::new(0),
        }
    }
}
struct Platform(Arc<Counts>);
struct Child(Arc<Counts>);
impl Launcher for Platform {
    fn launch(&self, bootstrap: Bootstrap) -> std::io::Result<Box<dyn Lifecycle>> {
        assert!(bootstrap.socket.is_absolute());
        assert_ne!(bootstrap.socket, bootstrap.data_socket);
        assert_eq!(bootstrap.token.len(), 64);
        self.0.launches.fetch_add(1, Ordering::SeqCst);
        self.0.launched.add_permits(1);
        Ok(Box::new(Child(Arc::clone(&self.0))))
    }
}
impl Lifecycle for Child {
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.0.stops.fetch_add(1, Ordering::SeqCst);
        })
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn plugin(directory: &std::path::Path) -> Plugin {
    Plugin {
        id: "bundled".into(),
        manifest: Manifest::default(),
        grants: BTreeSet::new(),
        program: None,
        arguments: Vec::new(),
        directory: directory.into(),
        policy: Policy::default(),
        handoff: None,
    }
}
async fn launched(counts: &Counts) {
    tokio::time::timeout(Duration::from_secs(3), counts.launched.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}
#[tokio::test]
async fn external_restart_and_shutdown_end_each_owned_generation() {
    let directory = tempfile::tempdir().unwrap();
    let counts = Arc::new(Counts::default());
    let service = Service::start_with_launcher(
        Hub::default(),
        plugin(directory.path()),
        Arc::new(Platform(Arc::clone(&counts))),
    )
    .unwrap();
    launched(&counts).await;
    assert!(service.restart());
    launched(&counts).await;
    assert_eq!(counts.stops.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    service.shutdown().await;
    assert_eq!(counts.launches.load(Ordering::SeqCst), 2);
    assert_eq!(counts.stops.load(Ordering::SeqCst), 2);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 2);
    assert!(!directory.path().join("bundled.sock").exists());
    assert!(!directory.path().join("bundled-data.sock").exists());
}
#[tokio::test]
async fn dropping_service_requests_stop_and_never_leaves_an_external_child() {
    let directory = tempfile::tempdir().unwrap();
    let counts = Arc::new(Counts::default());
    let service = Service::start_with_launcher(
        Hub::default(),
        plugin(directory.path()),
        Arc::new(Platform(Arc::clone(&counts))),
    )
    .unwrap();
    let mut events = service.events();
    launched(&counts).await;
    drop(service);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if *events.borrow_and_update() == Event::Stopped
                && counts.drops.load(Ordering::SeqCst) == 1
            {
                break;
            }
            if events.changed().await.is_err() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(counts.stops.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn external_registration_without_explicit_launcher_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    assert!(Service::start(Hub::default(), plugin(directory.path())).is_err());
}
