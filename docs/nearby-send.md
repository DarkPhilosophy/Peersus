# Devices nearby (experimental)

**Status: implemented behind one switch that is off by default, on Linux and Android. Experimental.** What was seen running and what was not is listed under [What was observed](#what-was-observed); nothing outside that list is claimed. How files go to a device held near was changed on 2026-11-11 at its owner's word (the other device must have Receive open, and then nothing more is pressed): that way of it is built and tested by the automated tests, and what was seen of it on devices is said in the same list.

The switch is **Settings → Device → Experimental → Devices nearby** (`nearby_send` in the settings, `--nearby-send` on the command line). It is shown only on Linux and Android, where there is an adapter for the platform's Bluetooth. Off: nothing is announced, nothing is listened for and nothing runs on a clock.

A device says one of two things to devices nearby, and they are different things, never said together:

- **"I am here, to be paired with"**, while its window shows Peers.
- **"I am here and I take files"**, while its window shows Receive.

On any other page it says neither. From that, three things follow, all over Bluetooth Low Energy announcements, with no Bluetooth connection and no Bluetooth pairing:

1. **Files listed on Send go at once to a paired device held near that has Receive open.** No destination is chosen and the circle is not held: opening Receive on the other device is what asks for the files. Its person is asked about each file there, as for any file.
2. **Devices nearby that are not paired and say they are there to be paired with are listed on Peers.** Choosing one begins the same pairing a code begins.
3. **Listed files are offered to a device that is not paired when it is brought near with Receive open.** Its person is asked about each file. Nobody is paired by it.

None of these changes how a transfer is authenticated or what pairing is: files still travel over the existing encrypted transport and the receiving side still accepts or refuses each one; a pairing begun from the list is still the ceremony in which both people choose the same number. The third does change who may *offer* a file: see [what an offer admits](#what-an-offer-admits).

## Why Bluetooth Low Energy

| Way | Why not, or why |
| --- | --- |
| NFC | Needs the two devices held back to back within a few centimetres, and a desktop has no reader. |
| Wi-Fi signal strength | A device cannot measure another device's signal without both being access point and client; the network's own strength says nothing of the distance between two clients. |
| Ultra-wideband | Measures distance properly, but few devices have it and there is no common interface across the platforms this application runs on. |
| Bluetooth Low Energy advertising | Every phone and most laptops have it; a device can announce some bytes and another can read them with the strength it heard them at, without connecting or pairing at the Bluetooth level. |

## What is announced

Two things, each as the data of a 128-bit service identifier of this application's own (`peersus_core::nearby::SERVICE` and `OFFER_SERVICE`).

### The tag (always while the switch is on)

An **8-byte tag**, in an announcement of the older kind that every Low Energy radio hears.

- The tag is the first 8 bytes of a hash, under a context string of its own, of the device's own peer identifier and the number of the current 15-minute period. The peer identifier is the public key the transport already authenticates. The hash has no secret key: two paired devices share no key outside a live connection. The tag is therefore a rotating pseudonym and nothing stronger: anyone who knows or can guess a device's identifier can compute its tags and recognise it.
- A device that listens computes the same tag for each of its **paired** devices, for the current period and the one on either side of it, and compares. A match names a paired device; anything else is ignored.
- The tag changes every 15 minutes, so a bystander cannot follow a device by a constant value. Someone who already knows the device's public key (a paired device, or a relay that has carried its traffic) can still recognise it; this is stated, not solved.
- The tag is a hint for where files go and never an authentication: a replayed or forged announcement can at worst make a paired device count as near that is not (see [what an offer admits](#what-an-offer-admits) for what follows from that). The connection that follows is the existing one, to that device's key, and a device that is not there does not answer.

### The offer (only while Peers or Receive is shown)

While a window of this device is open and in front with the switch on, and it shows Peers or Receive, the device also announces an **offer**: at most 200 bytes, in an announcement of the extended kind (Bluetooth 5), because the older kind carries 31 bytes in all.

| Bytes | What |
| --- | --- |
| 1 | What the device takes, one thing: `1` to be paired with (its window shows Peers), `2` files (its window shows Receive). Builds of 2026-11-11 before this change said `2` for both at once; they are not published and are not read differently. |
| 1 | Length of the name. |
| up to 32 | The device's name as its owner set it, cut at a whole character. |
| the rest, up to 166 | An invitation in its radio form (`Invitation::to_radio`): public identity (32), one-use capability (32), expiry (8), and the routes that fit, the relays first, then IPv4, then IPv6. |

- The invitation is of the same kind as the one behind the code on Peers: one use, five minutes at most, revoked when the network changes. Each of the two things has **its own gate** in the owner (`Invitations::create_nearby`), apart from the gate of a code a person is showing on the screen, so announcing never revokes such a code, nor the other way. It is made anew when it was used, when it was revoked, and a minute and a half before it runs out; the one before stays valid until used or run out.
- A window says it is open every 4 seconds (`seek-nearby`), on whatever page it shows, and says with it what that page lets the device be found for: to be paired with, files, or nothing. It says so at once when the page changes. The owner stops listening 10 seconds after the last word. What drives the radio asks the owner twice a second what to announce, so devices nearby are told of a page opened or left within about half a second of the announcement changing. What was announced for one thing is taken back when the window says the other or neither, and when the window goes behind another or away, so an invitation announced while Receive was shown admits no file after the person left Receive or put the application away. A window that is behind another, or closed, announces nothing. On a phone, a page that lies under the Settings overlay is not the page shown.
- A device listens for offers while its own window is open, on any page. It reads an offer only if it reads as one and its invitation is live. Of a device that is **not paired** it holds the offer (at most 16; one that is no longer heard leaves after 8 seconds). Of a **paired** device it keeps only what the offer says the device takes, for 2.5 seconds after it was last heard: the name and the invitation in it are of no use, since a send to a paired device goes over the connection that authenticates it. A device that leaves Receive simply stops saying it, so for up to those 2.5 seconds, and the half second above, listed files can still go to a paired device whose person has just left Receive.

### What an offer admits

An announced invitation admits for the one thing it was announced for, once:

- **A pairing** (offer `1`), with the capability as announced. What follows is the ceremony. The capability for files made from it opens nothing.
- **Files, without pairing** (offer `2`), with a capability derived from the announced one by a hash under a context of its own (`Invitation::for_files`). The device that announced admits it on a gate of its own (`Purpose::NearbyFiles`), takes file offers from that connection and nothing else, at most sixteen, and does not keep the sender as a device it knows. Each file waits for its person to accept or refuse it, with the name the sender gave itself beside it. A sender that is answered with anything other than "for files" sends nothing. The capability as announced opens nothing: a device that says it takes files cannot be paired with by what it announced.

Being used makes the next announcement carry a new invitation; the one before stays valid until it is used or runs out, as for a code, so at most two are out at a time.

**What this changes about an invitation, plainly.** A code on a screen is read only by who sees the screen. An offer is read by every radio in range, some tens of metres. Therefore:

- While a window shows Peers with the switch on, anyone in radio range who runs this protocol can *begin* a pairing with this device; while it shows Receive, anyone in range can *offer* it up to sixteen files. On any other page neither. The person then sees the pairing request and must choose a circle for it to go on, or sees each file and must accept it for it to be received; what nobody expected is refused by not choosing, or by Reject and Refuse. Nothing is written to the disk before a file is accepted. With *Save incoming files on their own* on, accepting still takes a press; only the choice of folder is skipped.
- The name in an offer, and the name beside an offered file, is whatever its maker wrote. Someone in range can announce under another device's name. If a person chooses that entry and goes through the ceremony, what stands between them and pairing with the wrong device is the ceremony alone: the other side has to choose the same of three circles. With a code read from the other device's own screen, the code itself says which device it is; here nothing does. Peers says under the list that a name is what the device says of itself.
- **Files sent to a device that is not paired go to whichever device the radio heard strongest, and nothing verifies which device that is.** A transmitter in range that announces loudly enough counts as near, and listed files are offered to it without a further press. The transport is encrypted to the key that device announced, so nobody else reads them; that device's person still has to accept. What the sender is protected by is narrow and stated: the files go only when exactly one such device was brought near after the files were listed, no destination was chosen, no paired device is near, and there are at most sixteen. Someone who must be sure of the receiver pairs first, or sends behind a code.
- **Files sent to a paired device held near go to that device and to no other, but what made them go can be forged.** That a paired device is near is said by its tag and that it has Receive open by its offer, and neither is authenticated: someone in range who knows a paired device's identifier can say both for it. The worst that follows is that listed files are sent, without a press on the sender, to the sender's own paired device, over the connection that authenticates that device; its person is asked about each. Files do not reach the forger by it.
- The offer carries the device's name, its public identity and the addresses it can be reached at (a relay, a local address) to radio range. The tag carries none of these.

This is why the offer is announced only while a window shows the page it is for, why the switch is off by default, and why the feature is marked experimental. Pairing by a code shown on one screen and read by the other remains, unchanged, and is the stronger way.

## "Near"

Bluetooth does not measure distance. What a listener has is the strength a packet was heard at (RSSI), which falls with distance but also changes with the antennas of both devices, a hand or a pocket in between, and reflections.

Two rules (`peersus_core::nearby::Rule`), each with two thresholds so that a device at the edge does not flicker:

| Rule | Near once | Gone once | For |
| --- | --- | --- | --- |
| Brought near | −55 dBm or stronger for 1.5 s unbroken (a silence of up to 2 s between two readings does not break it), or the strength the person set in place of −55 | 15 dB weaker than that, or unheard, for 3 s | Sending listed files to a paired device that has Receive open; offering them to a device that is not paired and has Receive open. |
| Around | −88 dBm or stronger for 1 s unbroken (a silence of up to 4 s does not break it) | weaker than −94 dBm, or unheard, for 8 s | Listing a device that is not paired on Peers. |

**The person sets how near "near" is, by measuring.** No threshold is right for every pair of devices, and the application does not guess one from a distance: under the switch, *How near is near* shows the threshold in use and has a *Measure* button. While measuring, the owner tells the strength of the device of this application that is heard strongest over the last second and a half (`measure-nearby`, said again every 4 seconds like `seek-nearby`). The person holds the two devices as near as they mean and presses *Set near from here*: the threshold becomes 8 dB under the strength shown (`nearby::near_at_from`), within −85 and −30 dBm, because the strength of a device held still moves by about that much from one reading to the next. *Reset* returns to −55 dBm. The setting is `nearby_near_at` (`--nearby-near-at`, 0 to reset) and is this device's own: each device of a pair is set on its own screen.

**Near is said by the tag, never by the offer.** A device that is not paired is listed by its offer, but whether it is near is judged from its tag, the same announcement a paired device is judged by and the one a person measures: its identity is in its offer, and its tag is made of that identity like anyone's. Measured between two phones held almost touching, one phone's offer was heard about 5 dB weaker than its tag from the same place, so a threshold set by the one would not hold for the other.

**A strength no radio hears at is no reading.** Android says 127 when it does not know the strength; taken as one it would be the strongest thing ever heard, would make a device near, and would set a threshold nothing can reach. Only strengths between −127 and 20 dBm are taken (`nearby::STRENGTHS`).

**What the measuring was needed for** (two phones, 2026-11-11, held by the owner; how far apart is the owner's word, nobody measured it). Held "almost touching, a small distance": one heard the other's tag at −63 to −67 dBm and was heard by it at −51 to −59 dBm; with the application's own −55 dBm the first never counted the second as near and the second did so only at moments. Held "very close" half an hour later, three runs of 25 s: the strongest over a second and a half was −49 dBm (median; −53 to −49) on the first and −48 dBm (−51 to −44) on the second, every single reading on the first was −53 dBm or stronger, and each counted the other as near with the application's −55 dBm. So −55 dBm holds for these two phones only when they are held against each other, with about 5 dB to spare, and a small distance more is enough to lose it; the two directions differ by 1 to 8 dB. That is why the person sets it, each device on its own screen. Between a phone and a workstation a few metres apart the strengths were around −70 dBm.

## What a person sees

- **Settings**: one switch, *Devices nearby*, in a section marked experimental; under it why nothing can be found when the platform said so (no Bluetooth, Bluetooth off, not allowed, refused), and *How near is near* (above). On Android the system's permission for nearby devices is asked for once each time the switch is turned on, with the window in front.
- **Send, a paired device**: with files listed and no destination chosen, they are sent at once to the paired device that is held near **and has Receive open**, and the page says so. The circle is not held and nothing is picked; the other person is asked about each file, in the application, as for any file. It is so for as long as it stays so: files listed after that go too. A paired device that is near with any other page open is given nothing; the page names it and says to open Receive on it. Two near with Receive open are given nothing, since two say nothing of which was meant; when only one of two has Receive open, that one said which. A destination picked by hand is never replaced: with one picked, the circle works as it always did and nearness does nothing.
- **Send, a device that is not paired**: with files listed (sixteen at most), no destination chosen and no paired device near, a device that has Receive open and is *brought* near is offered them at once, and the page says so. Bringing it near is the gesture: a device that was near with Receive open already when the files were listed is given nothing, and has to be taken away and brought again, or have its Receive closed and opened. Two brought at once are given nothing. The files leave the list, and their progress is shown in the circle as for any send.
- **Receive**: nothing is added to the page. Having it open is what says "I take files"; what arrives is asked about as always.
- **Peers**: a section *Nearby* with the devices nearby that are not paired and say they are there to be paired with, by the names they give: a column at the right on a desktop, and under the Pair and Connect buttons on a phone. Choosing one begins pairing; both devices then show the three circles, as after a code. Both devices need the switch on and Peers open. A device that has Receive open is not listed: it said another thing. Pair and Connect stay where they were.

## Platforms

| Platform | Announce | Listen |
| --- | --- | --- |
| Android (12 and later) | Tag: `BluetoothLeAdvertiser.startAdvertising`. Offer: `startAdvertisingSet` with the older kind turned off; a phone without extended advertising, or with too short a maximum, refuses and says so. Permission `BLUETOOTH_ADVERTISE`. | `BluetoothLeScanner` with a filter on each service's data and extended announcements included. Permission `BLUETOOTH_SCAN`, declared as never used for location. |
| Linux | BlueZ over D-Bus: two `org.bluez.LEAdvertisement1` objects of kind `broadcast` registered with `LEAdvertisingManager1`; the offer's asks for the secondary channel `1M`, which is what makes it the extended kind. Both ask for an interval of 200 to 250 ms (`MinInterval`, `MaxInterval`): left to itself BlueZ sends one every 1.28 s. | A BlueZ discovery session limited to Low Energy, with `DuplicateData`, reading `ServiceData` and `RSSI` of `org.bluez.Device1` as BlueZ says them. |
| Windows, macOS | None. The switch is not shown. | None. |

The tag is announced while the switch is on and the application can receive. Listening, for tags and for offers, runs only while a window is in front, on any page. An offer is announced only while that window shows Peers (to be paired with) or Receive (files). Nothing is listened for in the background on a phone.

## Layers

- `peersus-core::nearby`: the tag, the form of an offer and the two things it can say (`Takes`), the two rules of nearness, the threshold a measurement gives, and what is told to windows (`Found`), as pure functions with tests. The switch and the threshold are in `Settings`.
- `peersus-net`: the radio form of an invitation (`Invitation::to_radio` / `from_radio`), the capability for files derived from it (`Invitation::for_files`), the gate of each thing that is announced (`Invitations::create_nearby`, `Announced`), and the send that goes only to a device that admitted it for files (`NetworkHost::send_nearby`).
- `peersus-app::nearby`: joins the rules to a platform's radio (`Radio`) and to the owner; `nearby::linux` is the BlueZ adapter. The owner keeps the invitations that were heard and never tells them to a window; a window gets names and identities only. The owner, not a window, decides what is announced from what the window says its page is, and refuses files for a device that is not near.
- `peersus-ui-android` with `NearbyRadio.java`: the Android adapter. No rule lives in an adapter.
- `peersus-ui`: the switch, the measuring, the sends on Send and the list on Peers, from the state. What a page lets the device be found for is said in one place (the window's beat). The rules of when files go are `View::receiving_near` (paired) and `View::brought_near` (not paired); `View::near_waiting` names the paired device that is near without Receive open.

Commands (see [local control](api/local-control.md#devices-nearby-experimental)): `seek-nearby` (a window is open, and what its page lets the device be found for), `measure-nearby` (a person sets how near near is), `pair-nearby` (begin pairing with a listed device), and `queue-files` with the destination `nearby` (offer files to a device brought near). A send to a paired device held near is an ordinary `queue-files` to that device. What drives the radio speaks to the owner with two commands that cannot be sent over the local control socket.

## What cannot be shown by tests here

- That a given threshold corresponds to a given distance: it does not, in general.
- Behaviour with a phone model that is not on the desk.
- Advertising while another application holds all advertising slots of the adapter, and coexistence with an active Bluetooth audio link.
- Battery cost: to be measured before the switch could ever be on by default.

## What was observed

On 2026-11-11, between this workstation (Linux, BlueZ 5.87, an adapter with extended advertising) and one Android 16 phone, both on the build of that day. The record with its limits is entry E-032 of the [engineering log](engineering-log.md).

The list below and the two paragraphs under it were seen with the builds from before the change of the same day (E-038), when one offer said both things at once on every page. They stand as what was seen of the radio, the list and the pairing; where they tell of when an offer is announced or of a proposal, [what is announced](#what-is-announced) and [what a person sees](#what-a-person-sees) say how it is now, and what was seen of that follows them.

- The switch in Settings on the workstation, and on the phone the system's question for nearby devices the first time it was turned on.
- BlueZ took the offer as a second advertisement beside the tag when Peers was opened in a window in front, and let go of it 9 s after the window left Peers.
- Each device heard the other's offer: 156 and 164 bytes of data from the workstation at the phone, which only the extended kind carries.
- The list on Peers: under the Pair and Connect buttons on the phone, a column at the right in the workstation's window.
- Pairing from the list, begun from the phone and begun from the workstation: the three numbers on both within a few seconds; the same number on both paired them and took each off the other's list; different numbers were refused on both and paired nobody.
- Measured at the phone, lying on the desk: a phone's offer every 0.29 s (median), the workstation's every 1.29 s before it asked for a shorter interval and every 0.26 s after (an advertisement without the interval, registered beside it, was still heard every 1.29 s); strengths between −58 and −76 dBm. Measured at the workstation: the phone's tag every 0.3 s (median), nine in ten silences under 0.85 s, the longest of 88 was 1.5 s.
- A scan on the phone that had run for 26 minutes heard between a quarter and a ninth of what a new one heard of the same announcements. The Android adapter starts its scan anew every five minutes. With that, over 43 minutes in front, the phone heard an announcement sent every 0.26 s about 217 times a minute in the first sixteen minutes and 214 times a minute from the 25th to the 43rd.
- In that run the phone heard nothing of the workstation for six minutes (the 17th to the 23rd) while it still heard other devices. **The cause was found later the same day:** on this workstation's adapter, an advertisement registered with BlueZ before a discovery starts is not sent at all for as long as the discovery runs, and is sent again when it stops; one registered after the discovery started is sent as asked. Shown with two advertisements of a script and BlueZ's own scan: 35 and 38 heard in 8 s before the scan, none of either during it, 30 and 32 after; and 39 and 38 in 10 s for two registered two seconds after a scan had started. In those six minutes a window of the application had come to the front and listened. The same thing had kept the workstation's own tag off the air whenever its window listened, and, once the offer was announced from every page, its offer too. The Linux adapter now gives BlueZ its advertisements again when it sees that a discovery began, its own or another program's (`nearby::linux`, `ready`). An earlier note here said the workstation listening was not the cause; the comparison it rested on never made the workstation listen, and was wrong.
- After that change, with a window of a test instance of the workstation in front and listening, the phone listed it by its name under Nearby as a device that takes files, and went on listing it.

**Before the change, the proposal on Send was seen** on one phone with the application's own −55 dBm, the other phone held against it by the owner: a file listed, the circle said *hold to send, 1 device* and the paired phone was marked *Brought near: hold the circle to send to it, or tap it to let it go*. The list was cleared and nothing was sent. The owner then tried it between the two phones and said that the nearness was found very well and that the rest was wrong: it worked while the other phone was not on Receive, and the circle still had to be held. That proposal no longer exists.

**Before the change, files to a device that is not paired were seen** from a test instance of the workstation (an identity of its own, paired with nobody) to one phone: a phone that had been near all along was offered nothing; the phone that came back after the file was listed was offered it within three seconds, its person was asked, and the accepted file arrived whole with nobody paired. Nothing was moved for it: the workstation's threshold was lowered by hand and the phone came back by its window leaving and returning.

**After the change** (E-038; the phone and the test instance of the workstation on the final build of that day unless said, the two paired for it, the threshold lowered to −85 dBm by hand on whichever sent, and nothing moved, so this shows the rule and the timing and nothing of how near two devices must be):

- **What a page announces.** The window on Send: the phone heard no offer. On Peers: the phone listed it under Nearby as *not paired* 2.0 s later, and a pairing begun there by its Pair button paired the two after the circles. On Receive: 0.7 s later the phone heard that it takes files, and Peers on the phone no longer listed it. (The phone was then on the first build of the change, twenty minutes older, which reads offers the same way.) A second phone that was not driven said that it takes files, was heard so by the window, and was not listed on the window's Peers; that phone's screen was not looked at.
- **Files from the workstation to the phone.** The phone on Send, a file listed in the window: nothing was sent, the circle said *no device picked*, and the page named the phone and said to open Receive on it. Receive opened on the phone: the file was sent with nothing pressed, 0.4, 4.0, 1.0 and 1.0 s later in four runs, and the phone showed, in the application, *Strain-Test wants to send you a file* with Cancel and Accept. Cancelled on the phone: both sides recorded it as cancelled. On the first build of the change two files sent this way were accepted on the phone and arrived whole, the same SHA-256 each, and with Receive still open the second went 0.6 s after it was listed.
- **Leaving Receive.** The phone went to Send: the window stopped holding it as taking files 2.9, 3.3 and 2.8 s later in three runs, and a file listed in the window after that was not sent, each time. With the first build of the change, which asked what to announce every 2 s and held what a paired device said for 4 s, that was 6.1 s, and the first sending 3.2 s; both were shortened for it.
- **The system's folder picker over the phone's application** (it asks where to save when no folder is set; seen on the first build of the change): the window stopped holding the phone as taking files at once, and held it again when the picker was gone.
- **Files from the phone to the workstation.** The window on Receive, a file picked in the phone's own file picker on Send: 2.5 s after it was picked the workstation had the offer, its window showed *Claude wants to send you a file* with Cancel and Accept, and the phone showed *Sending* and that it was sent to the device held near with Receive open. Nothing was held. Another paired phone, near by the lowered threshold and saying nothing of taking files, was offered nothing.
- The window's drawing thread rested at 0 to 1 % of a processor through all of it (17 % once, during a send).

**Not observed after the change:** the same between two phones, which is what the owner asked for, by who built it: the second phone is the owner's and is not driven. A device really carried near at the application's own −55 dBm. Files to a device that is **not** paired with Receive as the gate (the rule and the gates are held by tests; on devices it was seen only before the change). Two paired devices near at once with Receive open. Accept in the workstation's window where it has to ask for a folder. Also not observed, as before: a phone without extended advertising, and anything on Windows or macOS, which have no adapter.
