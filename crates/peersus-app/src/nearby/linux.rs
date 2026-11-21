//! This computer's Bluetooth Low Energy, through BlueZ on the system bus, as the radio nearness is found with.
//!
//! It announces by registering `org.bluez.LEAdvertisement1` objects of kind `broadcast` (not connectable, no name)
//! whose only content is a service's data: one for the tag, and one of the extended kind for an offer to pair, which is
//! longer than the older kind carries. It listens with a discovery session limited to Low Energy. All belong to this
//! module's own connection to the bus: BlueZ ends them when that connection closes, so nothing is left announced by
//! an owner that is gone.
//!
//! BlueZ does not hand over announcements. It keeps one object for each device it hears and says when a property of it
//! changes; a reading is made here from each such word about a device whose service data is this application's. Asked
//! for with `DuplicateData`, BlueZ says the service data again each time it hears it, which is what makes a device that
//! stands still go on being heard.
//!
//! Nothing here decides who a device is or what near means, and nothing is asked on a clock: the caller says how things
//! should be, and each call looks once at whether the adapter is powered.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::StreamExt;
use peersus_core::nearby::{OFFER_SERVICE, SERVICE, TAG_BYTES, Trouble};
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant as OwnedValue, Variant as Value};

use super::{Kind, Radio, Reading, kind_of};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const ADVERTISING: &str = "org.bluez.LEAdvertisingManager1";
const DEVICE: &str = "org.bluez.Device1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const OBJECTS: &str = "org.freedesktop.DBus.ObjectManager";
/// Where this module's two advertisement objects are served, on its own connection.
const TAG_AT: &str = "/ro/peersus/nearby/advertisement";
const OFFER_AT: &str = "/ro/peersus/nearby/offer";
/// How long after BlueZ refused before it is asked again: a refusal is not answered by asking every second.
const AGAIN_AFTER: Duration = Duration::from_secs(30);
/// The most devices of this application's remembered at once, and the most readings kept between two looks.
const MOST_DEVICES: usize = 64;
const MOST_HEARD: usize = 256;
/// How often an advertisement is sent, in milliseconds, as the least and the most BlueZ may choose. Left to itself BlueZ
/// sends one every 1.28 s (measured from a phone), and one missed is then a silence longer than the rules of nearness
/// take for unbroken; a phone sends about four a second, and this asks for the same.
const EVERY_MS: (u32, u32) = (200, 250);

fn at(kind: Kind) -> &'static str {
    match kind {
        Kind::Tag => TAG_AT,
        Kind::Offer => OFFER_AT,
    }
}

type Properties = HashMap<String, OwnedValue>;

/// The data of this application's services in a device's `ServiceData`: of each of the two it has, the tag's first.
/// Nothing when the value is not what BlueZ documents; data longer than its kind is announced with is nobody's.
fn ours(service_data: &OwnedValue) -> Vec<(Kind, Vec<u8>)> {
    let Some(all) = service_data
        .try_clone()
        .ok()
        .and_then(|all| Properties::try_from(all).ok())
    else {
        return Vec::new();
    };
    let mut found: Vec<(Kind, Vec<u8>)> = all
        .into_iter()
        .filter_map(|(service, data)| {
            let (kind, most) = kind_of(&service)?;
            let data = Vec::<u8>::try_from(data).ok()?;
            (data.len() <= most).then_some((kind, data))
        })
        .collect();
    found.sort_by_key(|(kind, _)| *kind == Kind::Offer);
    found
}

#[derive(Default)]
struct Device {
    /// What it last announced of this application's.
    said: Vec<(Kind, Vec<u8>)>,
    strength: Option<i16>,
}

/// What BlueZ said of the devices that announce this application's services, and the readings made from it.
#[derive(Default)]
struct Devices {
    known: HashMap<String, Device>,
    heard: VecDeque<Reading>,
}

impl Devices {
    /// BlueZ said `properties` of the device at `path`. `now` is whether it said so as it heard the device (a reading)
    /// or was asked what it holds (not one: what it holds may be old).
    fn told(&mut self, path: &str, properties: &Properties, now: bool) {
        if let Some(service_data) = properties.get("ServiceData") {
            let said = ours(service_data);
            if said.is_empty() {
                self.known.remove(path);
            } else {
                if !self.known.contains_key(path) && self.known.len() >= MOST_DEVICES {
                    // Addresses change and old objects linger: what is still there says its data again.
                    self.known.clear();
                }
                self.known.entry(path.to_owned()).or_default().said = said;
            }
        }
        let Some(device) = self.known.get_mut(path) else {
            return;
        };
        if let Some(strength) = properties
            .get("RSSI")
            .and_then(|strength| i16::try_from(strength).ok())
        {
            device.strength = Some(strength);
        }
        if let (true, Some(strength)) = (now, device.strength) {
            for (kind, data) in &device.said {
                if self.heard.len() >= MOST_HEARD {
                    self.heard.pop_front();
                }
                self.heard.push_back(Reading {
                    kind: *kind,
                    data: data.clone(),
                    strength,
                });
            }
        }
    }
}

/// What one advertisement announces, and whether BlueZ let go of it by itself.
#[derive(Clone, Default)]
struct Spot {
    data: Arc<Mutex<Vec<u8>>>,
    released: Arc<AtomicBool>,
}

impl Spot {
    fn service_data(&self, service: &str) -> HashMap<String, Value<'static>> {
        let data = self
            .data
            .lock()
            .map_or_else(|_| Vec::new(), |data| data.clone());
        HashMap::from([(service.to_owned(), Value::from(data))])
    }
}

/// The advertisement of the tag, as BlueZ reads it when it is registered: a broadcast of the service's data and
/// nothing else, of the older kind every radio hears.
struct Advertisement(Spot);

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    /// BlueZ let go of the advertisement by itself (the adapter was powered off or removed).
    fn release(&self) {
        self.0.released.store(true, Ordering::SeqCst);
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> &str {
        "broadcast"
    }

    #[zbus(property)]
    fn service_data(&self) -> HashMap<String, Value<'static>> {
        self.0.service_data(SERVICE)
    }

    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        EVERY_MS.0
    }

    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        EVERY_MS.1
    }
}

/// The advertisement of an offer to pair: the same, of the other service, and of the extended kind, which a secondary
/// channel asks BlueZ for and without which so much data is not sent.
struct Offer(Spot);

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Offer {
    fn release(&self) {
        self.0.released.store(true, Ordering::SeqCst);
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> &str {
        "broadcast"
    }

    #[zbus(property)]
    fn service_data(&self) -> HashMap<String, Value<'static>> {
        self.0.service_data(OFFER_SERVICE)
    }

    #[zbus(property)]
    fn secondary_channel(&self) -> &str {
        "1M"
    }

    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        EVERY_MS.0
    }

    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        EVERY_MS.1
    }
}

#[derive(Default)]
struct Held {
    spot: Spot,
    served: bool,
}

/// The connection to the bus and what is held through it.
struct Link {
    connection: zbus::Connection,
    adapter: OwnedObjectPath,
    tag: Held,
    offer: Held,
    devices: Arc<Mutex<Devices>>,
    watcher: Option<tokio::task::JoinHandle<()>>,
}

impl Link {
    fn held(&self, kind: Kind) -> &Held {
        match kind {
            Kind::Tag => &self.tag,
            Kind::Offer => &self.offer,
        }
    }

    fn held_mut(&mut self, kind: Kind) -> &mut Held {
        match kind {
            Kind::Tag => &mut self.tag,
            Kind::Offer => &mut self.offer,
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        // The watcher holds the connection: ended here, the connection closes and BlueZ lets go of what it held for it.
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
    }
}

/// What is announced of one kind now, and when BlueZ last refused it.
#[derive(Default)]
struct Out {
    data: Option<Vec<u8>>,
    refused: Option<Instant>,
}

/// See the module's words.
pub struct BlueZ {
    /// The bus name BlueZ answers at; another only in tests.
    service: String,
    /// A bus to use instead of the system's; only in tests.
    bus: Option<zbus::Connection>,
    link: Option<Link>,
    tag: Out,
    offer: Out,
    listening: bool,
    /// Whether BlueZ said a discovery runs, of anyone's, when it was last asked (see [`Self::ready`]).
    discovering: bool,
    /// How many looks in a row BlueZ said nothing is being discovered while this listens.
    quiet: u8,
    listen_refused: Option<Instant>,
}

impl Default for BlueZ {
    fn default() -> Self {
        Self {
            service: BLUEZ.to_owned(),
            bus: None,
            link: None,
            tag: Out::default(),
            offer: Out::default(),
            listening: false,
            discovering: false,
            quiet: 0,
            listen_refused: None,
        }
    }
}

fn lately(refused: Option<Instant>) -> bool {
    refused.is_some_and(|at| at.elapsed() < AGAIN_AFTER)
}

/// What a refusal of BlueZ is to a person.
///
/// The bus's own refusals (its policy does not let this user speak to BlueZ) arrive as the library's named kind and not as a
/// name to compare; BlueZ's arrive by name.
fn refusal(error: &zbus::Error) -> Trouble {
    match error {
        zbus::Error::FDO(denied) if matches!(**denied, zbus::fdo::Error::AccessDenied(_)) => {
            Trouble::NotAllowed
        }
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.bluez.Error.NotAuthorized" =>
        {
            Trouble::NotAllowed
        }
        zbus::Error::MethodError(name, _, _) if name.as_str() == "org.bluez.Error.NotReady" => {
            Trouble::Off
        }
        _ => Trouble::Failed,
    }
}

impl BlueZ {
    #[cfg(test)]
    fn on(bus: zbus::Connection, service: &str) -> Self {
        Self {
            service: service.to_owned(),
            bus: Some(bus),
            ..Self::default()
        }
    }

    async fn call<B>(
        &self,
        connection: &zbus::Connection,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        connection
            .call_method(
                Some(self.service.as_str()),
                path,
                Some(interface),
                method,
                body,
            )
            .await
    }

    /// The first adapter BlueZ has that can announce.
    async fn adapter(&self, connection: &zbus::Connection) -> Option<OwnedObjectPath> {
        let reply = self
            .call(connection, "/", OBJECTS, "GetManagedObjects", &())
            .await
            .ok()?;
        let body = reply.body();
        let objects: HashMap<OwnedObjectPath, HashMap<String, Properties>> =
            body.deserialize().ok()?;
        objects
            .into_iter()
            .filter(|(_, interfaces)| {
                interfaces.contains_key(ADAPTER) && interfaces.contains_key(ADVERTISING)
            })
            .map(|(path, _)| path)
            .min_by(|one, other| one.as_str().cmp(other.as_str()))
    }

    fn out(&mut self, kind: Kind) -> &mut Out {
        match kind {
            Kind::Tag => &mut self.tag,
            Kind::Offer => &mut self.offer,
        }
    }

    /// Let go of everything: what BlueZ held for this connection ends with it.
    fn forget(&mut self) {
        self.link = None;
        self.tag.data = None;
        self.offer.data = None;
        self.listening = false;
        self.discovering = false;
    }

    /// Let go of everything once nothing is announced and nothing is listened for.
    fn forget_if_idle(&mut self) {
        if self.tag.data.is_none() && self.offer.data.is_none() && !self.listening {
            self.forget();
        }
    }

    /// The link when the adapter can be used now, made if there is none. Looks once at whether the adapter is powered.
    ///
    /// Gives whether BlueZ says a discovery runs, of anyone's: when none does, this connection's own has ended.
    async fn ready(&mut self) -> Result<bool, Trouble> {
        if self.link.is_none() {
            let connection = match &self.bus {
                Some(bus) => bus.clone(),
                None => zbus::Connection::system()
                    .await
                    .map_err(|_| Trouble::NoRadio)?,
            };
            let adapter = self.adapter(&connection).await.ok_or(Trouble::NoRadio)?;
            self.link = Some(Link {
                connection,
                adapter,
                tag: Held::default(),
                offer: Held::default(),
                devices: Arc::default(),
                watcher: None,
            });
        }
        let Some(link) = &self.link else {
            return Err(Trouble::NoRadio);
        };
        let said = self
            .call(
                &link.connection,
                link.adapter.as_str(),
                PROPERTIES,
                "GetAll",
                &(ADAPTER,),
            )
            .await
            .ok()
            .and_then(|reply| {
                let body = reply.body();
                let all: Properties = body.deserialize().ok()?;
                let flag = |name: &str| all.get(name).and_then(|value| bool::try_from(value).ok());
                Some((flag("Powered")?, flag("Discovering").unwrap_or(false)))
            });
        match said {
            Some((true, discovering)) => {
                // A discovery began since this was last asked, this connection's or another program's. What was
                // announced before it is given to BlueZ again: on the adapter this was measured with, an advertisement
                // registered before a discovery started was not sent at all for as long as the discovery ran, and one
                // registered after it started was sent as asked.
                if discovering && !self.discovering {
                    self.tag.data = None;
                    self.offer.data = None;
                }
                self.discovering = discovering;
                Ok(discovering)
            }
            Some((false, _)) => {
                // The discovery ended with the power. BlueZ may still hold the advertisements: they are given again when the power is back.
                self.stop_watching();
                self.tag.data = None;
                self.offer.data = None;
                self.listening = false;
                self.discovering = false;
                Err(Trouble::Off)
            }
            None => {
                // The adapter went, or BlueZ did: looked for again at the next call.
                self.forget();
                Err(Trouble::NoRadio)
            }
        }
    }

    fn stop_watching(&mut self) {
        if let Some(link) = &mut self.link {
            if let Some(watcher) = link.watcher.take() {
                watcher.abort();
            }
            if let Ok(mut devices) = link.devices.lock() {
                *devices = Devices::default();
            }
        }
    }

    async fn unregister(&self, kind: Kind) {
        if let Some(link) = &self.link
            && let Ok(path) = ObjectPath::try_from(at(kind))
        {
            let _ = self
                .call(
                    &link.connection,
                    link.adapter.as_str(),
                    ADVERTISING,
                    "UnregisterAdvertisement",
                    &(path,),
                )
                .await;
        }
    }

    async fn register(&mut self, kind: Kind, data: &[u8]) -> Result<(), Trouble> {
        let Some(link) = &mut self.link else {
            return Err(Trouble::NoRadio);
        };
        let connection = link.connection.clone();
        let adapter = link.adapter.clone();
        let held = link.held_mut(kind);
        if let Ok(mut said) = held.spot.data.lock() {
            *said = data.to_vec();
        }
        if !held.served {
            let server = connection.object_server();
            let spot = held.spot.clone();
            match kind {
                Kind::Tag => server.at(at(kind), Advertisement(spot)).await.map(|_| ()),
                Kind::Offer => server.at(at(kind), Offer(spot)).await.map(|_| ()),
            }
            .map_err(|_| Trouble::Failed)?;
            held.served = true;
        }
        held.spot.released.store(false, Ordering::SeqCst);
        let path = ObjectPath::try_from(at(kind)).map_err(|_| Trouble::Failed)?;
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        self.call(
            &connection,
            adapter.as_str(),
            ADVERTISING,
            "RegisterAdvertisement",
            &(path, options),
        )
        .await
        .map(|_| ())
        .map_err(|error| refusal(&error))
    }

    /// Announce `data` as the advertisement of `kind`, or stop announcing it.
    async fn put(&mut self, kind: Kind, data: Option<&[u8]>) -> Result<(), Trouble> {
        let Some(data) = data else {
            if self.out(kind).data.take().is_some() {
                self.unregister(kind).await;
            }
            self.out(kind).refused = None;
            self.forget_if_idle();
            return Ok(());
        };
        self.ready().await?;
        if self
            .link
            .as_ref()
            .is_some_and(|link| link.held(kind).spot.released.load(Ordering::SeqCst))
        {
            self.out(kind).data = None;
        }
        if self.out(kind).data.as_deref() == Some(data) {
            return Ok(());
        }
        if lately(self.out(kind).refused) {
            return Err(Trouble::Failed);
        }
        // Whatever BlueZ still holds of this connection's is taken back first: it reads the data only when it is given.
        self.out(kind).data = None;
        if self
            .link
            .as_ref()
            .is_some_and(|link| link.held(kind).served)
        {
            self.unregister(kind).await;
        }
        match self.register(kind, data).await {
            Ok(()) => {
                let out = self.out(kind);
                out.data = Some(data.to_vec());
                out.refused = None;
                Ok(())
            }
            Err(trouble) => {
                self.out(kind).refused = Some(Instant::now());
                tracing::warn!(?kind, ?trouble, "Nearness could not be announced");
                Err(trouble)
            }
        }
    }

    /// Start a discovery session of this connection's own, and the task that makes readings of what BlueZ then says.
    async fn discover(&mut self) -> Result<(), Trouble> {
        let Some(link) = &self.link else {
            return Err(Trouble::NoRadio);
        };
        let connection = link.connection.clone();
        let adapter = link.adapter.clone();
        let devices = link.devices.clone();
        // Listened to before anything is asked for, so that nothing said in between is lost.
        let rule = |interface: &'static str, member: &'static str| {
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(self.service.as_str())
                .interface(interface)
                .member(member)
        };
        let changed = rule(PROPERTIES, "PropertiesChanged")
            .path_namespace(adapter.as_str())
            .arg(0, DEVICE)
            .build()
            .map_err(|_| Trouble::Failed)?;
        let added = rule(OBJECTS, "InterfacesAdded")
            .build()
            .map_err(|_| Trouble::Failed)?;
        let queued = std::num::NonZeroUsize::new(MOST_HEARD);
        let changed = zbus::MessageStream::for_match_rule(changed, &connection, queued)
            .await
            .map_err(|_| Trouble::Failed)?;
        let added = zbus::MessageStream::for_match_rule(added, &connection, queued)
            .await
            .map_err(|_| Trouble::Failed)?;
        // What BlueZ already holds: the data and the last strength of devices it heard before this listened. Not readings.
        if let Ok(reply) = self
            .call(&connection, "/", OBJECTS, "GetManagedObjects", &())
            .await
        {
            let body = reply.body();
            if let Ok(objects) =
                body.deserialize::<HashMap<OwnedObjectPath, HashMap<String, Properties>>>()
                && let Ok(mut devices) = devices.lock()
            {
                for (path, interfaces) in &objects {
                    if let Some(properties) = interfaces.get(DEVICE) {
                        devices.told(path.as_str(), properties, false);
                    }
                }
            }
        }
        // Low Energy only. `DuplicateData` has the service data said again each time it is heard; a strength asked for,
        // however weak, has every change of strength said and not only the large ones.
        let filter: HashMap<&str, Value<'_>> = HashMap::from([
            ("Transport", Value::from("le")),
            ("DuplicateData", Value::from(true)),
            ("RSSI", Value::from(-127_i16)),
        ]);
        self.call(
            &connection,
            adapter.as_str(),
            ADAPTER,
            "SetDiscoveryFilter",
            &(filter,),
        )
        .await
        .map_err(|error| refusal(&error))?;
        match self
            .call(
                &connection,
                adapter.as_str(),
                ADAPTER,
                "StartDiscovery",
                &(),
            )
            .await
        {
            Ok(_) => {}
            // This connection's own session is already there: that is what was asked for.
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.InProgress" => {}
            Err(error) => return Err(refusal(&error)),
        }
        let watcher = tokio::spawn(watch(changed, added, devices));
        if let Some(link) = &mut self.link {
            link.watcher = Some(watcher);
        } else {
            watcher.abort();
        }
        Ok(())
    }
}

/// Make readings of what BlueZ says of devices for as long as it says anything.
async fn watch(
    mut changed: zbus::MessageStream,
    mut added: zbus::MessageStream,
    devices: Arc<Mutex<Devices>>,
) {
    loop {
        tokio::select! {
            message = changed.next() => {
                let Some(Ok(message)) = message else { return };
                let header = message.header();
                let Some(path) = header.path() else { continue };
                let body = message.body();
                let Ok((interface, properties, _)) = body.deserialize::<(String, Properties, Vec<String>)>() else { continue };
                if interface == DEVICE && let Ok(mut devices) = devices.lock() {
                    devices.told(path.as_str(), &properties, true);
                }
            }
            message = added.next() => {
                let Some(Ok(message)) = message else { return };
                let body = message.body();
                let Ok((path, interfaces)) = body.deserialize::<(OwnedObjectPath, HashMap<String, Properties>)>() else { continue };
                if let Some(properties) = interfaces.get(DEVICE) && let Ok(mut devices) = devices.lock() {
                    devices.told(path.as_str(), properties, true);
                }
            }
        }
    }
}

impl Radio for BlueZ {
    async fn announce(&mut self, tag: Option<[u8; TAG_BYTES]>) -> Result<(), Trouble> {
        self.put(Kind::Tag, tag.as_ref().map(<[u8; TAG_BYTES]>::as_slice))
            .await
    }

    async fn offer(&mut self, offer: Option<&[u8]>) -> Result<(), Trouble> {
        self.put(Kind::Offer, offer).await
    }

    async fn listen(&mut self, on: bool) -> Result<(), Trouble> {
        if !on {
            if std::mem::take(&mut self.listening) {
                if let Some(link) = &self.link {
                    let _ = self
                        .call(
                            &link.connection,
                            link.adapter.as_str(),
                            ADAPTER,
                            "StopDiscovery",
                            &(),
                        )
                        .await;
                }
                self.stop_watching();
            }
            self.listen_refused = None;
            self.forget_if_idle();
            return Ok(());
        }
        let discovering = self.ready().await?;
        if self.listening {
            // BlueZ ends a discovery when the power goes, and a power that went and came back between two looks is not seen here
            // as off. A few looks with nothing discovered is that: listened for again.
            self.quiet = if discovering {
                0
            } else {
                self.quiet.saturating_add(1)
            };
            if self.quiet < 3 {
                return Ok(());
            }
            self.stop_watching();
            self.listening = false;
        }
        self.quiet = 0;
        if lately(self.listen_refused) {
            return Err(Trouble::Failed);
        }
        match self.discover().await {
            Ok(()) => {
                self.listening = true;
                self.listen_refused = None;
                Ok(())
            }
            Err(trouble) => {
                self.stop_watching();
                self.listen_refused = Some(Instant::now());
                tracing::warn!(?trouble, "Nearness could not be listened for");
                Err(trouble)
            }
        }
    }

    fn heard(&mut self) -> Vec<Reading> {
        self.link
            .as_ref()
            .and_then(|link| {
                link.devices
                    .lock()
                    .ok()
                    .map(|mut devices| devices.heard.drain(..).collect())
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nearby::MOST_TAG;
    use peersus_core::nearby::OFFER_BYTES;

    fn value<'a>(value: impl Into<Value<'a>>) -> OwnedValue {
        OwnedValue::try_from(value.into()).unwrap()
    }

    fn service_data(service: &str, data: &[u8]) -> OwnedValue {
        value(HashMap::from([(
            service.to_owned(),
            Value::from(data.to_vec()),
        )]))
    }

    fn said(properties: impl IntoIterator<Item = (&'static str, OwnedValue)>) -> Properties {
        properties
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect()
    }

    fn tagged(data: &[u8], strength: i16) -> Reading {
        Reading {
            kind: Kind::Tag,
            data: data.to_vec(),
            strength,
        }
    }

    #[test]
    fn a_reading_is_made_of_each_word_about_a_device_of_ours_and_of_no_other() {
        let mut devices = Devices::default();
        // What BlueZ holds when asked is remembered and is not a reading.
        devices.told(
            "/dev/a",
            &said([
                ("ServiceData", service_data(SERVICE, &[1; 8])),
                ("RSSI", value(-60_i16)),
            ]),
            false,
        );
        assert!(devices.heard.is_empty());
        // The data said again with no strength: heard at the strength last said.
        devices.told(
            "/dev/a",
            &said([("ServiceData", service_data(SERVICE, &[1; 8]))]),
            true,
        );
        // A change of strength alone: the data last said.
        devices.told("/dev/a", &said([("RSSI", value(-48_i16))]), true);
        // Another service's device, and a device that said nothing of any service, are nobody.
        devices.told(
            "/dev/b",
            &said([
                (
                    "ServiceData",
                    service_data("0000fe2c-0000-1000-8000-00805f9b34fb", &[9; 8]),
                ),
                ("RSSI", value(-30_i16)),
            ]),
            true,
        );
        devices.told("/dev/c", &said([("RSSI", value(-30_i16))]), true);
        assert_eq!(
            devices.heard.drain(..).collect::<Vec<_>>(),
            vec![tagged(&[1; 8], -60), tagged(&[1; 8], -48)]
        );
        // The service's name in capitals is the same service; a device that stops announcing it is forgotten.
        devices.told(
            "/dev/d",
            &said([
                (
                    "ServiceData",
                    service_data(&SERVICE.to_ascii_uppercase(), &[2; 8]),
                ),
                ("RSSI", value(-40_i16)),
            ]),
            true,
        );
        devices.told(
            "/dev/a",
            &said([(
                "ServiceData",
                service_data("0000fe2c-0000-1000-8000-00805f9b34fb", &[1; 8]),
            )]),
            true,
        );
        devices.told("/dev/a", &said([("RSSI", value(-20_i16))]), true);
        assert_eq!(
            devices.heard.drain(..).collect::<Vec<_>>(),
            vec![tagged(&[2; 8], -40)]
        );
        // An offer to pair is the other service's data, far longer than a tag; a device that says both at once is
        // read as both, and what is longer than an offer is nobody's.
        let offer = vec![7; OFFER_BYTES];
        devices.told(
            "/dev/e",
            &said([
                ("ServiceData", service_data(OFFER_SERVICE, &offer)),
                ("RSSI", value(-70_i16)),
            ]),
            true,
        );
        devices.told(
            "/dev/f",
            &said([
                (
                    "ServiceData",
                    value(HashMap::from([
                        (OFFER_SERVICE.to_owned(), Value::from(offer.clone())),
                        (SERVICE.to_owned(), Value::from(vec![3_u8; 8])),
                    ])),
                ),
                ("RSSI", value(-71_i16)),
            ]),
            true,
        );
        devices.told(
            "/dev/g",
            &said([
                (
                    "ServiceData",
                    service_data(OFFER_SERVICE, &vec![7; OFFER_BYTES + 1]),
                ),
                ("RSSI", value(-30_i16)),
            ]),
            true,
        );
        let offered = |strength| Reading {
            kind: Kind::Offer,
            data: offer.clone(),
            strength,
        };
        assert_eq!(
            devices.heard.drain(..).collect::<Vec<_>>(),
            vec![offered(-70), tagged(&[3; 8], -71), offered(-71)]
        );
    }

    #[test]
    fn what_is_not_as_documented_or_too_long_is_nobody_and_nothing_grows_without_bound() {
        let mut devices = Devices::default();
        devices.told(
            "/dev/a",
            &said([("ServiceData", value("text")), ("RSSI", value(-30_i16))]),
            true,
        );
        devices.told(
            "/dev/a",
            &said([
                ("ServiceData", service_data(SERVICE, &[0; MOST_TAG + 1])),
                ("RSSI", value(-30_i16)),
            ]),
            true,
        );
        // A strength that is not the number BlueZ documents is not a reading.
        devices.told(
            "/dev/b",
            &said([
                ("ServiceData", service_data(SERVICE, &[3; 8])),
                ("RSSI", value("strong")),
            ]),
            true,
        );
        assert!(devices.heard.is_empty());
        for index in 0..MOST_DEVICES * 3 {
            devices.told(
                &format!("/dev/{index}"),
                &said([
                    ("ServiceData", service_data(SERVICE, &[4; 8])),
                    ("RSSI", value(-50_i16)),
                ]),
                true,
            );
            for _ in 0..4 {
                devices.told(
                    &format!("/dev/{index}"),
                    &said([("RSSI", value(-51_i16))]),
                    true,
                );
            }
        }
        assert!(devices.known.len() <= MOST_DEVICES);
        assert_eq!(devices.heard.len(), MOST_HEARD);
    }

    /// One advertisement as the fake holds it: its kind, its data, and the secondary channel it asked for, if any.
    type Advertised = (String, Vec<(Kind, Vec<u8>)>, Option<String>);

    /// What a stand-in for BlueZ was asked and what it answers.
    #[derive(Default)]
    struct Said {
        powered: bool,
        discovering: bool,
        refuse: bool,
        filter: Option<(String, bool)>,
        started: u32,
        stopped: u32,
        registered: u32,
        unregistered: u32,
        /// What is advertised now, by the object it was read from.
        advertised: HashMap<String, Advertised>,
    }

    type Fake = Arc<Mutex<Said>>;

    const HCI: &str = "/org/bluez/hci0";
    const HELD: &str = "/org/bluez/hci0/dev_held";

    struct FakeObjects(Fake);

    #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
    impl FakeObjects {
        fn get_managed_objects(&self) -> HashMap<OwnedObjectPath, HashMap<String, Properties>> {
            let (powered, discovering) = {
                let said = self.0.lock().unwrap();
                (said.powered, said.discovering)
            };
            HashMap::from([
                (
                    OwnedObjectPath::try_from(HCI).unwrap(),
                    HashMap::from([
                        (
                            ADAPTER.to_owned(),
                            said([
                                ("Powered", value(powered)),
                                ("Discovering", value(discovering)),
                            ]),
                        ),
                        (ADVERTISING.to_owned(), Properties::new()),
                    ]),
                ),
                (
                    OwnedObjectPath::try_from(HELD).unwrap(),
                    HashMap::from([(
                        DEVICE.to_owned(),
                        said([
                            ("ServiceData", service_data(SERVICE, &[5; 8])),
                            ("RSSI", value(-70_i16)),
                        ]),
                    )]),
                ),
            ])
        }
    }

    struct FakeAdapter(Fake);

    #[zbus::interface(name = "org.bluez.Adapter1")]
    impl FakeAdapter {
        #[zbus(property)]
        fn powered(&self) -> bool {
            self.0.lock().unwrap().powered
        }

        #[zbus(property)]
        fn discovering(&self) -> bool {
            self.0.lock().unwrap().discovering
        }

        fn set_discovery_filter(&self, filter: Properties) {
            let transport = filter
                .get("Transport")
                .and_then(|transport| transport.try_clone().ok())
                .and_then(|transport| String::try_from(transport).ok())
                .unwrap_or_default();
            let duplicates = filter
                .get("DuplicateData")
                .and_then(|duplicates| bool::try_from(duplicates).ok())
                .unwrap_or(false);
            self.0.lock().unwrap().filter = Some((transport, duplicates));
        }

        fn start_discovery(&self) {
            let mut said = self.0.lock().unwrap();
            said.started += 1;
            said.discovering = true;
        }

        fn stop_discovery(&self) {
            let mut said = self.0.lock().unwrap();
            said.stopped += 1;
            said.discovering = false;
        }
    }

    struct FakeAdvertising(Fake);

    #[zbus::interface(name = "org.bluez.LEAdvertisingManager1")]
    impl FakeAdvertising {
        /// As BlueZ does: what is advertised is read from the caller's object when it is given, and not again.
        async fn register_advertisement(
            &self,
            advertisement: OwnedObjectPath,
            _options: Properties,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> zbus::fdo::Result<()> {
            if self.0.lock().unwrap().refuse {
                return Err(zbus::fdo::Error::AccessDenied("refused".to_owned()));
            }
            let failed = |error: zbus::Error| zbus::fdo::Error::Failed(error.to_string());
            let reply = connection
                .call_method(
                    None::<&str>,
                    advertisement.as_str(),
                    Some(PROPERTIES),
                    "GetAll",
                    &("org.bluez.LEAdvertisement1",),
                )
                .await
                .map_err(failed)?;
            let body = reply.body();
            let all: Properties = body.deserialize().map_err(failed)?;
            let text = |name: &str| {
                all.get(name)
                    .and_then(|value| value.try_clone().ok())
                    .and_then(|value| String::try_from(value).ok())
            };
            let data = all.get("ServiceData").map(ours).unwrap_or_default();
            let mut said = self.0.lock().unwrap();
            said.registered += 1;
            said.advertised.insert(
                advertisement.to_string(),
                (
                    text("Type").unwrap_or_default(),
                    data,
                    text("SecondaryChannel"),
                ),
            );
            Ok(())
        }

        fn unregister_advertisement(&self, advertisement: OwnedObjectPath) {
            let mut said = self.0.lock().unwrap();
            said.unregistered += 1;
            said.advertised.remove(advertisement.as_str());
        }
    }

    /// What was heard, waited for briefly: BlueZ's words arrive on a task of their own.
    async fn hears(radio: &mut BlueZ) -> Vec<Reading> {
        for _ in 0..300 {
            let heard = radio.heard();
            if !heard.is_empty() {
                return heard;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Vec::new()
    }

    #[tokio::test]
    async fn the_adapter_is_asked_what_the_caller_said_and_made_so_again_when_the_system_undid_it()
    {
        let fake = Fake::default();
        fake.lock().unwrap().powered = true;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (bluez, bus) = tokio::try_join!(
            zbus::connection::Builder::unix_stream(theirs)
                .server("0123456789abcdef0123456789abcdef")
                .p2p()
                .serve_at("/", FakeObjects(fake.clone()))
                .serve_at(HCI, FakeAdapter(fake.clone()))
                .serve_at(HCI, FakeAdvertising(fake.clone()))
                .build(),
            zbus::connection::Builder::unix_stream(ours).p2p().build(),
        )
        .unwrap();
        let mut radio = BlueZ::on(bus, BLUEZ);
        let asked = |fake: &Fake| {
            let said = fake.lock().unwrap();
            (
                said.registered,
                said.unregistered,
                said.started,
                said.stopped,
            )
        };

        // Announced once, as a broadcast of the tag; the same word again asks nothing of BlueZ.
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        assert_eq!(asked(&fake), (1, 0, 0, 0));
        let advertised = |fake: &Fake, at: &str| fake.lock().unwrap().advertised.get(at).cloned();
        assert_eq!(
            advertised(&fake, TAG_AT),
            Some((
                "broadcast".to_owned(),
                vec![(Kind::Tag, vec![7; TAG_BYTES])],
                None
            ))
        );
        // BlueZ reads the data only when it is given: another tag is given anew.
        radio.announce(Some([8; TAG_BYTES])).await.unwrap();
        assert_eq!(asked(&fake), (2, 1, 0, 0));
        assert_eq!(
            advertised(&fake, TAG_AT),
            Some((
                "broadcast".to_owned(),
                vec![(Kind::Tag, vec![8; TAG_BYTES])],
                None
            ))
        );
        // An offer to pair is another advertisement beside it, of the extended kind; the tag's is not touched, and
        // taking the offer back leaves the tag announced.
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        assert_eq!(asked(&fake), (3, 1, 0, 0));
        assert_eq!(
            advertised(&fake, OFFER_AT),
            Some((
                "broadcast".to_owned(),
                vec![(Kind::Offer, vec![4; OFFER_BYTES])],
                Some("1M".to_owned())
            ))
        );
        assert!(advertised(&fake, TAG_AT).is_some());
        radio.offer(None).await.unwrap();
        radio.offer(None).await.unwrap();
        assert_eq!(asked(&fake), (3, 2, 0, 0));
        assert_eq!(advertised(&fake, OFFER_AT), None);
        assert!(advertised(&fake, TAG_AT).is_some());

        // Listened for on Low Energy with the data said again each time. What BlueZ already held is not a reading.
        radio.listen(true).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(asked(&fake), (3, 2, 1, 0));
        assert_eq!(fake.lock().unwrap().filter, Some(("le".to_owned(), true)));
        assert!(radio.heard().is_empty());
        // A word about the device it held is a reading, with the data it held.
        bluez
            .emit_signal(
                None::<&str>,
                HELD,
                PROPERTIES,
                "PropertiesChanged",
                &(
                    DEVICE,
                    said([("RSSI", value(-50_i16))]),
                    Vec::<String>::new(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(hears(&mut radio).await, vec![tagged(&[5; 8], -50)]);
        // A device it hears for the first time is one too.
        bluez
            .emit_signal(
                None::<&str>,
                "/",
                OBJECTS,
                "InterfacesAdded",
                &(
                    ObjectPath::try_from("/org/bluez/hci0/dev_new").unwrap(),
                    HashMap::from([(
                        DEVICE.to_owned(),
                        said([
                            ("ServiceData", service_data(SERVICE, &[6; 8])),
                            ("RSSI", value(-44_i16)),
                        ]),
                    )]),
                ),
            )
            .await
            .unwrap();
        assert_eq!(hears(&mut radio).await, vec![tagged(&[6; 8], -44)]);

        // Bluetooth switched off is said, and nothing held from before is taken as still so when it is back.
        {
            let mut said = fake.lock().unwrap();
            said.powered = false;
            said.discovering = false;
        }
        assert_eq!(radio.listen(true).await, Err(Trouble::Off));
        assert_eq!(
            radio.announce(Some([8; TAG_BYTES])).await,
            Err(Trouble::Off)
        );
        fake.lock().unwrap().powered = true;
        radio.announce(Some([8; TAG_BYTES])).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(asked(&fake), (4, 3, 2, 0));

        // A discovery BlueZ ended without a word is started again after a few looks, not at the first.
        fake.lock().unwrap().discovering = false;
        radio.listen(true).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(asked(&fake).2, 2);
        radio.listen(true).await.unwrap();
        assert_eq!(asked(&fake).2, 3);

        // A refusal is said as what it is once, and BlueZ is not asked again at every look.
        fake.lock().unwrap().refuse = true;
        assert_eq!(
            radio.announce(Some([9; TAG_BYTES])).await,
            Err(Trouble::NotAllowed)
        );
        assert_eq!(
            radio.announce(Some([9; TAG_BYTES])).await,
            Err(Trouble::Failed)
        );
        assert_eq!(asked(&fake), (4, 4, 3, 0));

        // Turned off: the discovery is stopped and nothing stays announced.
        fake.lock().unwrap().refuse = false;
        radio.announce(Some([9; TAG_BYTES])).await.unwrap_err();
        radio.listen(false).await.unwrap();
        radio.announce(None).await.unwrap();
        assert_eq!(asked(&fake), (4, 4, 3, 1));
        assert!(fake.lock().unwrap().advertised.is_empty());
        assert!(radio.heard().is_empty());
    }

    #[tokio::test]
    async fn what_was_announced_before_a_discovery_began_is_given_again_once_it_runs() {
        let fake = Fake::default();
        fake.lock().unwrap().powered = true;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (_bluez, bus) = tokio::try_join!(
            zbus::connection::Builder::unix_stream(theirs)
                .server("0123456789abcdef0123456789abcdef")
                .p2p()
                .serve_at("/", FakeObjects(fake.clone()))
                .serve_at(HCI, FakeAdapter(fake.clone()))
                .serve_at(HCI, FakeAdvertising(fake.clone()))
                .build(),
            zbus::connection::Builder::unix_stream(ours).p2p().build(),
        )
        .unwrap();
        let mut radio = BlueZ::on(bus, BLUEZ);
        let given = |fake: &Fake| {
            let said = fake.lock().unwrap();
            (said.registered, said.unregistered)
        };
        // One look: the tag, the offer, and then the listening, in the order the rules ask for them.
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(given(&fake), (2, 0));
        // The next look: the discovery runs now, and both are taken back and given again, with what they said.
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(given(&fake), (4, 2));
        assert_eq!(fake.lock().unwrap().advertised.len(), 2);
        // And then left alone for as long as the discovery goes on.
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        radio.listen(true).await.unwrap();
        assert_eq!(given(&fake), (4, 2));
        // Another program's discovery, begun while this one does not listen, is one too.
        radio.listen(false).await.unwrap();
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        assert_eq!(given(&fake), (4, 2));
        fake.lock().unwrap().discovering = true;
        radio.announce(Some([7; TAG_BYTES])).await.unwrap();
        radio.offer(Some(&[4; OFFER_BYTES])).await.unwrap();
        assert_eq!(given(&fake), (6, 4));
    }
}
