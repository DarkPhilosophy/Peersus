# macOS: portability and how it could be tested

Status (2026-11-12): **built and tested in a macOS guest (14.8.9, x86-64, software graphics); never run as a window, never on Apple Silicon, never on a Mac.** The first version of this page (2026-11-10) was read from source only; where it was wrong or has since been tried, the section "Tried in a guest" says what was seen. Statements about Apple's toolchain that were not tried are marked *inferred*.

## What the source does for macOS today

### Would stop a build through the project's entry points

- `./dev.sh desktop` and `./build.sh desktop` end in the `xtask` step that compiles the native extension helper (`crates/peersus-plugin-runtime/native/host.c`). That step runs on every Unix (`cfg!(unix)`), so macOS is not refused. **Tried: the step fails there**, not for the reason first inferred: the file itself compiles under `-Wall -Wextra -Werror` (Apple's headers do define `MSG_NOSIGNAL`), but `-pie` is reported as unused and `-Werror` turns that into an error, and Apple's linker rejects `-Wl,-z,relro,-z,now` (`ld: unknown options: -z -z`). **Fixed (E-147):** `xtask` leaves both out on Apple (programs there are position independent by default; there is no relocation-read-only option); `cargo xtask build` then passed in the guest (release, 34 min on 4 virtual CPUs) and made a Mach-O helper, and the staged bundle carries it. Plain `cargo build -p peersus-app` does not compile that file.
- The renderer is built from source (`FORCE_SKIA_BUILD=1`). **Tried: it builds on macOS** (13 min 24 s on 4 virtual CPUs for the `peersus-ui` development build). Its one missing prerequisite there was `ninja`, which macOS and its Command Line Tools do not provide; the prerequisites are written down for Linux only (`scripts/install-linux-deps.sh`).
- Nothing in the Rust crates stopped compilation: `cargo metadata`, `cargo check` of the interface-independent crates and of `peersus-app`, and the development builds of the owner (with the test identity) and of `peersus-ui` all passed. One test file did not compile (see below). The vendored window and toolkit patches were not read for macOS.

### Would compile and then refuse, or do less

| Facility | On macOS, as read in source |
| --- | --- |
| Replaying a pointer and keyboard (`peersus.input`) | Every replay is refused as unsupported (`desktop_input.rs`) |
| A device's own pointer, drawn apart | Linux only; not compiled |
| Monitors and their arrangement | Nothing enumerates them, so the device tells its peers of none; no refusal says so |
| Pointer and keyboard carried across screen edges | Not started |
| Telling peers the computer sleeps | Not started |
| Following the system's light or dark theme | No watcher; the toolkit's default stays |
| Clipboard change events | None: the clipboard is read by looking at it again, not on a change |
| Re-opening a file being sent | The `/proc` path does not exist; falls back to a copy |
| Where data is kept | **Seen:** `~/.local/share/peersus` (the Linux convention), not `~/Library/Application Support` |
| Packaging | No `.app` bundle, disk image, signing or notarisation anywhere in the repository |
| Automated checks | Every workflow runs on Ubuntu; none on a macOS runner |

### Has a macOS path that has never been exercised

The keychain for the device identity and relay token (`keyring` with its Apple backend), the local control socket with its same-user check, file locking, the no-replace publish of a received file by hard link, ending a plugin's process group, file dialogs, the text clipboard, network interface watching (the vendored `netwatch` has a macOS backend), recognising a Mach-O extension program, and a window without the system's frame whose controls the application draws. Each compiles from a branch that includes macOS; none has run there. The length limit of a Unix socket path on macOS was not checked against the paths used.

## Tried in a guest

The owner decided to run the guest (it is not Apple hardware; see the licence paragraph below) and accepted Apple's licence in it himself. Guest: macOS 14.8.9 (23J631), x86-64, 4 CPUs, 8 GiB, from `dockur/macos` under rootless Podman with KVM; software graphics. The sources were copied to the guest's own disk (without build output, version control or the VM images); Rust is the version the repository pins (1.96.1) through rustup; Apple's Command Line Tools 16.2.

| Step | Result |
| --- | --- |
| `cargo metadata` | passes, 19 s |
| `cargo check` of `peersus-api`, `-core`, `-net`, `-plugin` | passes, 75 s |
| `cargo check -p peersus-app` | passes, 71 s |
| Owner built with `--features test-identity` | builds; started; its control socket and preferences appeared under `~/.local/share/peersus` |
| `cargo build -p peersus-ui` (Skia from source) | builds, 13 min 24 s; needed `ninja` |
| Unit and integration tests of `-api`, `-core`, `-net`, `-plugin`, `-app` | **one compile error, six failures, all explained** (below); everything else passed |
| `cargo test -p peersus-ui --lib` | 648 passed, 0 failed, 2 ignored, 25 s |
| The screens opened, pressed and drawn with no display (`--features headless`, the gate's own step) | 15 passed, 0 failed |
| Native helper step of `xtask` | fails (above) |
| `peersus-ui` started in the logged-in desktop session | **aborts at once**: `freya-winit … metal.rs:66: No Metal-capable device found`, a `panic` that cannot unwind. The guest has no GPU device. A real Mac always has one, so this says nothing against Apple hardware, but the program has no fallback and no sentence for a machine without one |

What the failures were:

- **A test helper did not compile** (`update_install.rs`): the process mask is 16 bits on Apple targets and 32 elsewhere (`mode_t`). Fixed in the test helper with an explicit conversion; the Linux lint passes with it.
- **Four tests bind `127.0.0.2`** (`runtime`, `live_settings`) and failed with *Could not bind the selected network*: on Linux all of `127.0.0.0/8` is the machine, on macOS only `127.0.0.1` is until the interface is given an alias. With `ifconfig lo0 alias 127.0.0.2` they passed. The product refusing an address the machine does not have is right; the tests assume Linux.
- **Four tests move eighty files** (`send_held` ×2, `peersus-ui` `link::tests` ×2). In E-042 they failed with *Too many open files* and passed with `ulimit -n 4096`. Run again on 2026-11-12 on a tree of E-043 (limit verified at 4096 inside the same process) they **still fail, now by time, not by descriptors**: the two `send_held` tests end with `Elapsed` (`the complete selection did not cross its single files code`) and a `QueueBusy` assertion after 190 to 290 s, and the two `link` tests end at their 20 s limits. With the 20 s limit of the collector raised to 300 s in the guest's copy only (restored afterwards; the repository was not touched), `one_desktop_code_delivers_eighty_paths…` **passed** but took over 60 s (the test run 128 s), while on the Linux host the whole library's 657 tests run in 18 s. `one_native_code_precedes_descriptors…` still failed, on `completed early batches ended the selection`: read in the source, that is by design, not a race: `await_picked` is given `patience` 20 s by the test, and after it the picker's own thread calls `finished_for`, which clears `pending`; the raised limit was only the collector's, so the producer's 20 s still ran out while eighty files took over a minute. So on this guest eighty files take more than a minute where Linux takes seconds: slow, not stuck, and the four tests assume a speed this guest does not have. **Why it is that slow was not found** (candidates, none checked: a flush to disk that macOS makes stronger than Linux does on a virtual disk, software networking of the guest, 4 shared CPUs); whether a real Mac sending many files is slow in the same way is **not known**. The program does not raise its own descriptor limit anywhere in the source.
- **Tools the guest script needs:** `~/bin` (holds `ninja`) must be on `PATH`, or any change to a file that makes cargo run the Skia build script again fails at once with *failed to run `ninja`*; and `ifconfig lo0 alias 127.0.0.2` for the four `127.0.0.2` tests.
- **The window was seen on macOS on 2026-11-12, with the toolkit's software renderer** (screenshots of the guest's display: `shots/macos_first_window.png`, `macos_name_typed.png`, `macos_send_current.png`, `macos_keychain_prompt.png`). What it took, in the order it was found:
  - *A logged-in desktop session.* Started over SSH with nobody logged in at the console, the program stays alive, shows nothing and its owner log says `Identity unavailable: Cannot save identity in system credential store`: an SSH session cannot reach the keychain the graphical login unlocks. The login was typed into the login window from the host (`virsh send-key`), and the program was started from that session by `open` on a `.command` file (`~/run-peersus.command` in the guest).
  - *`FREYA_RENDERER=software`.* Without it, in the logged-in session, the program aborts exactly as in E-042: `No Metal-capable device found` (`freya-winit` `drivers/metal.rs:66`, from `AppWindow::new`). With it both processes run (the window and the owner) and the window draws. **The Metal path was never seen working**; a guest has no Metal device. The program does not fall back by itself: a Mac without Metal gets an abort, not a message.
  - *What was done in the window:* first run asked for the device's name; a click put the caret in the field, five keys typed `macvm`, `Continue` went to the Send page; on a second start the name was kept. The brand, the pages in the bar, the state (`CONNECTED`), the three window buttons drawn by the application and the Send page of the current tree (no numbered labels) were all seen. In the bar `RECEI…` and `TRANS…` are cut at this window width.
  - *The keychain asks again after every rebuild.* The first start of a rebuilt, unsigned owner showed the system's prompt *"peersus wants to use your confidential information stored in "Peersus" in your keychain. The authenticity of "peersus" cannot be verified"*, with the login keychain's password required; the window appeared only after `Allow`. An item is tied to the program that made it, and an unsigned build is a different program each time. A signed release should not ask (not verified: nothing is signed).
  - *What misled earlier the same day, kept as a warning:* the guest's display, read with `virsh screenshot`, runs 20 to 40 s behind (compare the clock in the picture with `date` over SSH), and for the first minutes after `virsh start` it still shows the Apple logo; two "logo only, the display does not work" conclusions and a swap of the display device to `vmware-svga` (undone) came from pictures taken too early. Clicks sent as absolute pointer events (`input-send-event` on the USB tablet) and keys (`send-key`) do arrive.

- **As an application, not a program in a terminal** (2026-11-12, asked for by the owner, who looked for Peersus in the guest and found none: until then it was a development build started from a launcher file). `scripts/stage-macos.py` (see [development](development.md#the-macos-bundle)) made `Peersus.app` from a release build in the guest; it was copied to `/Applications` and opened with `open`. Seen (`shots/macos_app_running.png`, `macos_app_keychain_prompt.png`, `macos_app_could_not_start.png`): the menu bar says **Peersus** where it said `peersus-ui`, the Dock shows the application's mark, `lsappinfo` names the bundle `io.github.DarkPhilosophy.Peersus`, the window and the owner run from `Contents/MacOS`, `codesign --verify --deep --strict` passes on the ad hoc seal, and the bundle's `LSEnvironment` gives the window the software renderer with no launcher. Its data is in `~/.local/share/peersus`, not in `~/Library/Application Support` (where a Mac user would look; not changed, noted).
  - *The keychain on a first start.* The dialog no longer says the program's authenticity cannot be verified (the ad hoc seal is enough for that sentence to go), and after **Always Allow** the next start asked nothing. A rebuilt bundle is another seal and will ask again; a release signed with one identity should not (not verified).
  - *Found: a first start that waits too long says the application could not start.* The window waits 90 s for the owner (`link.rs`, a wait made that long for credential dialogs). The keychain dialog stood unanswered for about two minutes while nobody was at the guest, and the window then said *Peersus could not start … it did not open its connection*, with the dialog still on the screen; answering it afterwards did not bring the window back, the application had to be opened again. A person who reads the dialog slowly, or is away, gets an error for a start that only waits for them. Not changed.

What this guest cannot show, and was not looked at: rendering through Metal, speed of drawing (software, in a guest, behind a lagging display), the file dialog, drag and drop, a transfer to another device, pairing (the guest is behind the virtual machine's address translation and listens on a port chosen at each start, so another device reaches it only through a relay; no attempt from a phone has reached its owner, see E-046), the extension overlay, a light theme, Apple Silicon, a disk image, a signature of an identity, notarization, a second display.

## How the guest was chosen

Two projects run macOS in a virtual machine inside a container on a Linux host with KVM. Read first, then `dockur/macos` was the one tried:

| | dockur/macos | sickcodes/Docker-OSX |
| --- | --- | --- |
| Maintenance | Active: pushed 2026-10-08, 9 open issues | Stalled: last push 2025-11-11, 419 open issues |
| Licence of the tool | MIT | GPL-3.0 |
| Host | KVM, a CPU with AVX2, 4 GB RAM and 32 GB disk at least; on AMD no more than 8 GB RAM during installation; no `--privileged` | KVM; asks for a kernel module setting on the host; its newer examples force an Intel CPU model |
| macOS versions | 11 to 15; 26 selectable and described by the project as very slow | 10.13 to 26; its last commit says 26 shows a white screen |
| Unattended installation | Advertised; its own steps still describe clicking through the installer | Only for a pre-installed 10.15 image; newer ones are installed by hand |
| Shell from the host | None documented; a web viewer and VNC | SSH forwarded on a port |
| Sharing a source tree | A host folder mounted in the guest (9p) | None documented in that direction |
| Graphics | Software; an opt-in path that translates Metal to Vulkan on the host was added 2026-09-28 | Software |

**The licence question came first.** dockur/macos's own README says Apple's licence does not permit installing macOS on hardware Apple did not sell, and tells its users to run it only on Apple hardware. The development workstation is not Apple hardware. The owner decided on 2026-11-11 to run the guest anyway and clicked Agree in the guest himself; that decision is his, and this page records it and does not take it. dockur/macos with macOS 14 was installed, and the unattended steps it advertises did not complete by themselves: Setup Assistant was finished by the owner at the web viewer, and Remote Login had to be switched on in System Settings (`systemsetup -setremotelogin` is refused without Full Disk Access).

Typing into the guest through the web viewer lost every character that needs Shift (a capital, `~`, `:`); lowercase, digits and `-` arrived. Commands that need them were therefore sent over SSH once it was on.

What was tried for a window: the guest was restarted with `GPU=Y` and `/dev/dri` (the host has an AMD GPU with a render node). With the default video model QEMU refused to start (`-device vmware: 'vmware' is not a valid device model name`); with `VGA=virtio-vga-gl` it started and macOS booted, but the guest still sees only a 7 MB paravirtual display (`Vendor ID 0x1af4`, no kernel extension) and `peersus-ui`, started in the logged-in session, aborts the same way (`No Metal-capable device found`) and macOS shows its own "quit unexpectedly" dialog. So no window was shown in this guest, and the vGPU route did not give it a Metal device with this host GPU, this QEMU and macOS 14. A hosted macOS runner or a Mac is what is left for a window.

What such a guest cannot show, whatever the result: behaviour on Apple Silicon (the guest is x86-64), real Metal rendering and its performance, real displays (HiDPI, several monitors), and anything that needs the keychain's prompts or an Apple account as a person meets them. A hosted macOS runner in CI is the other route for build and unit tests on Apple hardware, and the only one for Apple Silicon; adding a workflow is a change to CI and needs the owner's approval.

## Assessment

macOS is **not a supported platform and not a candidate for a first release**, but it is no longer unknown: everything in the Rust workspace that was tried compiles there, the renderer builds from source, and 648 interface tests, the display-less window tests and the tests of the other crates pass (with two machine settings the tests assume; see above). What stops it, in the order a person would meet it: the native helper step of `./dev.sh desktop` / `./build.sh desktop` fails on Apple's compiler and linker flags, so the project's own entry point cannot build there; the program aborts without a Metal device and says nothing a person could read; it keeps its data in `~/.local/share`; and the list above (input, displays, clipboard events, sleep, theme, packaging, signing) is the rest of the work. The first two are small and were not changed here because they need a Mac or a runner to check the result.
