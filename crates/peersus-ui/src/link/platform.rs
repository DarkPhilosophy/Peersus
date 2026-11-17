//! What a platform offers the window beyond itself, as plain values the window calls: pickers, a camera, its switches and settings,
//! a folder it receives into, the ways to open an address or an extension's file. Empty on a desktop.
use super::*;

/// How often the interface looks for what a platform's picker or camera has handed over. The platform answers on a thread of its own
/// at a moment nobody can predict, and the interface has no timer, so it looks at this rhythm: fast enough that a choice shows at
/// once, slow enough to cost nothing. A rhythm of the interface and not a wait of the person's.
pub const LOOK_EVERY: std::time::Duration = std::time::Duration::from_millis(150);

/// How long a wait on a picker (of files or of a folder) goes on before it is given up as unanswered. Ten minutes is far more than
/// choosing takes, and it ends a wait that nothing will answer, so that no thread waits for ever.
pub const PICKER_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a camera is left on for a code before it is closed as unpointed. Two minutes is far more than reading a code takes.
pub const CAMERA_PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// A platform's file picker, for a platform with no file dialog the toolkit can open (Android).
///
/// It hands over files and not bytes or paths: the system's picker returns a handle that only the system's own
/// calls can read, so the platform opens the file and gives the program the open file, which is kept (see
/// [`peersus_net::held::Held`]) and sent from where it is. `open` shows the picker and returns at once; what was
/// chosen arrives in `picked`, which the interface asks for until the picker says it is done.
#[derive(Clone)]
pub struct FilePicker {
    /// Show the picker, for one file or for several. Does nothing the interface waits on.
    pub open: Arc<dyn Fn(bool) + Send + Sync>,
    /// What the picker handed over, and whether it is still open.
    pub picked: peersus_net::picked::Picked,
}

impl PartialEq for FilePicker {
    /// The same picker is the same bridge to the platform: the call that opens it is shared, and compared by
    /// identity. A closure cannot be compared by what it does, and the picked state is the same for the same bridge.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// A platform's camera, for reading the invitation of another device (Android, where every phone has one).
///
/// It gives frames and nothing else: what becomes of them is [`crate::scanner::Scanner`], tried without a phone.
/// `open` starts the camera for a scan and returns at once; the frames reach that scan on the camera's own thread.
/// `close` ends it. `refused` says, once, that the permission was refused, so that the circle can say so and not
/// wait for ever for frames that will not come.
#[derive(Clone)]
pub struct CameraControl {
    /// Start the camera for this scan. Anything open before is closed, and its scan ended.
    pub open: Arc<dyn Fn(crate::scanner::Scanner) + Send + Sync>,
    /// Close the camera and end the scan in progress. Safe when nothing is open.
    pub close: Arc<dyn Fn() + Send + Sync>,
    /// Whether the camera said it could not be used, taken once.
    pub refused: Arc<dyn Fn() -> bool + Send + Sync>,
    /// How many quarter turns clockwise a frame must be turned to stand upright for the person holding the phone now
    /// (the sensor's mounting and the screen's turn, see [`crate::preview::turns_to_upright`]). Asked afresh each time,
    /// since the phone may be turned while the camera is open.
    pub turns: Arc<dyn Fn() -> u32 + Send + Sync>,
}

impl PartialEq for CameraControl {
    /// The same camera is the same bridge: compared by the identity of the call that opens it.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// A platform's notifications, which the person can allow or block outside the program (Android): the program cannot know by itself
/// whether an offer that arrives will be shown, so Settings asks the platform, and offers the way to allow them.
#[derive(Clone)]
pub struct NotificationsControl {
    /// Whether the notifications of this program can be shown now. Asked afresh each time it is drawn, since the person may change it
    /// in the system's settings and come back.
    pub allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Ask for the permission if the system will still ask, and otherwise open the settings where it can be given. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for NotificationsControl {
    /// The same notifications are the same bridge: compared by the identity of the call that opens the settings.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// The permission to draw the panel of an offer over other applications (Android), which the person gives by hand on a screen of the system
/// and the program cannot ask for with a dialog: Settings asks the platform whether it is given, and offers the way to that screen.
#[derive(Clone)]
pub struct OverlayControl {
    /// Whether the panel can be drawn over other applications now. Asked afresh each time it is drawn, since the person gives it on a screen
    /// of the system and comes back.
    pub allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Open the screen of the system where the person allows it. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for OverlayControl {
    /// The same permission is the same bridge: compared by the identity of the call that opens the screen.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// Whether the system leaves this program out of its battery optimisation (Android), which decides if the background service keeps its
/// network while the phone is idle. The person chooses it on a screen of the system: Settings shows the state and offers the way there.
#[derive(Clone)]
pub struct BatteryControl {
    /// Whether the program is left out of the optimisation now. Asked afresh while the row is on the screen, since the person decides it on
    /// a screen of the system and comes back.
    pub unrestricted: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Open the screen of the system where the person chooses. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for BatteryControl {
    /// The same choice is the same bridge: compared by the identity of the call that opens the screen.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.open, &other.open)
    }
}

/// A setting of the platform that exists only to serve one of the application's services to extensions: a consent or a recovery the
/// system asks the person for before the application can do, in the background, what that service offers (Android). It is the
/// platform's own and grants nothing to anyone by itself.
///
/// Settings shows it only while an installed extension imports `service` from the application (see `view::wants_service`): with no such
/// extension it would be a row about nothing. What it is called and found by is the adapter's to say, so the interface names no service
/// and no provider of its own.
#[derive(Clone)]
pub struct ServiceSetting {
    /// The application's service this setting serves: what an extension's manifest imports.
    pub service: &'static str,
    /// What the row is called, in English: the interface translates it as it does its own titles.
    pub title: &'static str,
    /// Further words a person may look for it by. Not translated.
    pub search: &'static str,
    /// The state now, in the platform's words. Asked afresh while the row is on the screen.
    pub status: Arc<dyn Fn() -> String + Send + Sync>,
    /// Open the platform's own screen or dialog for it. Returns at once.
    pub open: Arc<dyn Fn() + Send + Sync>,
    pub switch: PlatformSwitch,
}

impl PartialEq for ServiceSetting {
    fn eq(&self, other: &Self) -> bool {
        self.service == other.service
            && self.title == other.title
            && Arc::ptr_eq(&self.open, &other.open)
            && self.switch == other.switch
    }
}

/// A platform setting with a real on/off state: `on` reads it, `set` asks the platform to change it (which may first ask the person).
#[derive(Clone)]
pub struct PlatformSwitch {
    pub on: Arc<dyn Fn() -> bool + Send + Sync>,
    pub set: Arc<dyn Fn(bool) + Send + Sync>,
}

impl PartialEq for PlatformSwitch {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.on, &other.on) && Arc::ptr_eq(&self.set, &other.set)
    }
}

/// The choice of which camera reads a code, for a phone that has several (Android): the camera the program picks is not always the one that
/// sees a screen well, so the person chooses, and sees the result in the circle of scanning.
#[derive(Clone)]
pub struct CameraChoice {
    /// The back cameras the phone has, one line each (id, focal length in tenths of a millimetre, nearest focus in centimetres, 1 if
    /// chosen), tab separated. Asked afresh each time it is drawn.
    pub list: Arc<dyn Fn() -> String + Send + Sync>,
    /// Choose a camera by its id; an empty id leaves the choice to the program. A camera that is open is started again on the new one.
    pub choose: Arc<dyn Fn(&str) + Send + Sync>,
}

impl PartialEq for CameraChoice {
    /// The same choice is the same bridge: compared by the identity of the call that chooses.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.choose, &other.choose)
    }
}

pub type AppearanceSetter = Arc<dyn Fn(&[u32; 9]) + Send + Sync>;

/// The colours the interface is drawn with now, which a platform draws its own surfaces with as well (Android: the notification of an offer),
/// so that they follow the theme the person chose and the one they made, and are not colours written in the platform's resources.
#[derive(Clone)]
pub struct Appearance {
    /// Tell the platform the colours in use, as 0xRRGGBB, in the order: surface, raised, border, text, muted, accent, bad, button,
    /// button text. Called when they change, and kept by the platform for the times the interface is not drawn.
    pub set: AppearanceSetter,
}

impl PartialEq for Appearance {
    /// The same appearance is the same bridge: compared by the identity of the call.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set, &other.set)
    }
}

/// What a button of the notification of an offer asked that only the window can do (Android): View files pressed with the window closed
/// brings the window forward, and the list of the files has to be open when it comes, or the person presses View files twice.
#[derive(Clone)]
pub struct OfferButtons {
    /// Whether View files was pressed since the last time this was asked, taken once.
    pub take_view: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The offers whose Accept was pressed on the notification or the panel and could not be served without a window (no folder is kept, or
    /// saving on their own is off), taken once: the window is brought forward for them and asks where each goes.
    pub take_accepts: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl PartialEq for OfferButtons {
    /// The same buttons are the same bridge: compared by the identity of the call.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.take_view, &other.take_view)
    }
}

/// A platform's way to put a received file in a folder the person chose, for a platform where such a folder is not a path
/// the program can write (Android): the system's picker gives an address that only the system's own calls can write through.
///
/// The program receives into `staging`, a folder it can write; the person's choice is asked for when they accept, and when the
/// file has arrived whole the platform puts it in that folder, then removes the staged copy. The address is text the program
/// never reads as a path.
#[derive(Clone)]
pub struct FolderSink {
    /// A folder the program can write, where an accepted file is received before it is put in the chosen one.
    pub staging: PathBuf,
    /// What the person answered, and the deliveries that finished.
    pub picked: peersus_net::folder_picked::FolderPicked,
    /// Show the platform's folder picker. Returns at once; the answer arrives in `picked`.
    pub choose: Arc<dyn Fn() + Send + Sync>,
    /// Put a finished transfer's file in the folder chosen for it. Returns at once; how it went arrives in `picked`.
    pub deliver: Arc<dyn Fn(&peersus_net::folder_picked::Handoff) + Send + Sync>,
    /// The folder chosen for the files that come in on their own, as the tree address the system gave, or `None`. Not a path: on this
    /// platform the setting of a folder that is a path cannot say it, so the platform keeps it and is asked.
    pub saved: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// Show the platform's folder picker for the saved folder. Returns at once; the platform keeps what is chosen.
    pub choose_saved: Arc<dyn Fn() + Send + Sync>,
    /// Let go of the saved folder: files that come in ask where they go again.
    pub forget_saved: Arc<dyn Fn() + Send + Sync>,
}

impl PartialEq for FolderSink {
    /// The same sink is the same bridge: compared by the identity of the call that opens the picker.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.choose, &other.choose)
    }
}

/// A platform-provided HTTPS browser launch. False means no launch was dispatched.
#[derive(Clone)]
pub struct UrlOpener(pub Arc<dyn Fn(&str) -> bool + Send + Sync>);
impl UrlOpener {
    pub fn open(&self, url: &str) -> bool {
        (self.0)(url)
    }
}
impl PartialEq for UrlOpener {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Opens the platform's package picker; its adapter stages the selected archive through the owner.
#[derive(Clone)]
pub struct PluginPicker(pub Arc<dyn Fn() -> Result<(), &'static str> + Send + Sync>);
impl PartialEq for PluginPicker {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Opens the platform's document picker for a language file; its adapter reads the chosen file and imports it through the
/// owner, and says itself what went wrong (Android, where a picked file has no path to read).
#[derive(Clone)]
pub struct LanguagePicker(pub Arc<dyn Fn() -> Result<(), &'static str> + Send + Sync>);
impl PartialEq for LanguagePicker {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Runs an extension's action on the file chosen on Send, through the owner in this process. Present on a platform whose
/// chosen file is a held handle and not a path (Android): the command that names a held file is trusted composition and is
/// never written to a frame, so a window that reaches its owner by frames cannot send it. Given the extension, the action,
/// and the file as a path or as the handle it is held under.
#[derive(Clone)]
pub struct SelectedAction(
    #[allow(clippy::type_complexity)]
    pub  Arc<
        dyn Fn(peersus_core::plugins::PluginId, String, Option<PathBuf>, Option<(u64, String)>)
            + Send
            + Sync,
    >,
);
impl PartialEq for SelectedAction {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Present on a platform with a system Back (Android): `presses` changes at each press the activity's back callback took, and
/// `leave` is what Back does when nothing in the window is open to close (the activity goes behind the others, as the system's own
/// Back). `by_key` is a system that gives Back to the window as a key instead (before Android 13): then the key is the press, and
/// otherwise a Back key that also arrives is the same press again and is not acted on twice.
#[derive(Clone)]
pub struct BackControl {
    pub presses: tokio::sync::watch::Receiver<u64>,
    pub leave: Arc<dyn Fn() + Send + Sync>,
    pub by_key: bool,
}
impl PartialEq for BackControl {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.leave, &other.leave)
    }
}

/// What a platform offers that the toolkit does not: each ability is present on the platform that needs it, and absent
/// where the toolkit already does the thing. One value, so that a fourth ability is a field and not another parameter
/// on every function that builds a window.
#[derive(Clone, Default)]
pub struct Platform {
    /// Present on a platform whose system draws bars over the window (Android); absent on a desktop, whose
    /// window manager already keeps the window clear of them.
    pub safe_area: Option<SafeAreaSource>,
    /// Present on a platform whose files are not paths (Android), where the toolkit's own dialog does not exist.
    pub file_picker: Option<FilePicker>,
    /// Present on a platform with a camera that the program can read frames from (Android).
    pub camera: Option<CameraControl>,
    /// Present on a platform where a folder the person chooses is not a path the program can write (Android).
    pub folder_sink: Option<FolderSink>,
    /// Present on a platform where the toolkit cannot say whether the system is in its dark mode (Android, where the window has no
    /// theme to give): the answer comes from the platform itself, and is what the first frame is drawn with.
    pub dark: Option<DarkSource>,
    /// Present on a platform whose notifications the person can block (Android): Settings shows their state and the way to allow them.
    pub notifications: Option<NotificationsControl>,
    /// Present on a platform with several cameras for the person to choose from (Android).
    pub camera_choice: Option<CameraChoice>,
    /// Present on a platform that draws surfaces of its own with the colours of the interface (Android).
    pub appearance: Option<Appearance>,
    /// Present on a platform whose notification of an offer has buttons that need the window (Android).
    pub offer_buttons: Option<OfferButtons>,
    /// Present on a platform where the panel of an offer is drawn over other applications by a permission the person gives by hand (Android).
    pub overlay: Option<OverlayControl>,
    /// Present on a platform where the system's battery optimisation can stop the background service from reaching the network (Android).
    pub battery: Option<BatteryControl>,
    pub open_url: Option<UrlOpener>,
    pub plugin_picker: Option<PluginPicker>,
    /// Present on a platform where a language file is chosen by the platform's picker and has no path (Android).
    pub language_picker: Option<LanguagePicker>,
    /// Present on a platform where the file chosen on Send is held and not a path (Android).
    pub selected_action: Option<SelectedAction>,
    /// Settings of the platform that each serve one of the application's services to extensions (Android); none on a desktop.
    pub service_settings: Vec<ServiceSetting>,
    /// The address the system opened this window with, when it was one: a store link (`peersus://store/<id>`) from a web page,
    /// shown once the window is attached. The desktop entry passes it as the window's argument; Android, from the intent that
    /// started the activity. An address that is not a store link shows nothing.
    pub opened_with: Option<String>,
    /// Present on a platform with a system Back (Android): what is open on top of the window closes first.
    pub back: Option<BackControl>,
}

impl PartialEq for Platform {
    /// The same platform is the same bridges: each ability is compared by the identity of the call behind it, since a
    /// call cannot be compared by what it does, and an absent ability equals only an absent one.
    fn eq(&self, other: &Self) -> bool {
        let same_area = match (&self.safe_area, &other.safe_area) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_dark = match (&self.dark, &other.dark) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_area
            && same_dark
            && self.file_picker == other.file_picker
            && self.camera == other.camera
            && self.folder_sink == other.folder_sink
            && self.notifications == other.notifications
            && self.camera_choice == other.camera_choice
            && self.appearance == other.appearance
            && self.offer_buttons == other.offer_buttons
            && self.overlay == other.overlay
            && self.battery == other.battery
            && self.open_url == other.open_url
            && self.plugin_picker == other.plugin_picker
            && self.language_picker == other.language_picker
            && self.selected_action == other.selected_action
            && self.service_settings == other.service_settings
            && self.opened_with == other.opened_with
            && self.back == other.back
    }
}
