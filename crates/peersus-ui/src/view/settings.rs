//! The Settings screen's rules: its tabs, the language list, the Binds page's rows, where notices go, the search through
//! settings and which group a scroll is in. Pure rules, no toolkit.
use super::*;

/// The tabs of the Settings screen, in the order they are listed. The screen opens on the first:
/// an overview of how things stand, before any of the places where they are changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Overview,
    Device,
    Appearance,
    /// The languages: the one shown, how far each is translated, downloading, importing and sharing one.
    Languages,
    Network,
    Extensions,
    Permissions,
    /// Key bindings. Desktop only: a phone has no such keys and does not list it.
    Binds,
    About,
}

impl SettingsTab {
    /// Every tab, in the order of the list.
    pub const ALL: [SettingsTab; 9] = [
        SettingsTab::Overview,
        SettingsTab::Device,
        SettingsTab::Appearance,
        SettingsTab::Languages,
        SettingsTab::Network,
        SettingsTab::Extensions,
        SettingsTab::Permissions,
        SettingsTab::Binds,
        SettingsTab::About,
    ];

    /// The tabs this platform lists: all of them on a desktop, all but Binds on Android.
    pub fn shown() -> &'static [SettingsTab] {
        Self::listed(cfg!(target_os = "android"))
    }

    pub(super) fn listed(phone: bool) -> &'static [SettingsTab] {
        const PHONE: [SettingsTab; 8] = [
            SettingsTab::Overview,
            SettingsTab::Device,
            SettingsTab::Appearance,
            SettingsTab::Languages,
            SettingsTab::Network,
            SettingsTab::Extensions,
            SettingsTab::Permissions,
            SettingsTab::About,
        ];
        if phone { &PHONE } else { &Self::ALL }
    }

    /// The name shown in the list.
    pub fn title(self) -> &'static str {
        match self {
            SettingsTab::Overview => "Overview",
            SettingsTab::Device => "Device",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Languages => "Languages",
            SettingsTab::Network => "Network",
            SettingsTab::Extensions => "Extensions",
            SettingsTab::Permissions => "Permissions",
            SettingsTab::Binds => "Binds",
            SettingsTab::About => "About",
        }
    }
}

/// The name English gives itself; every other language's name is the one its file gives.
pub const ENGLISH_NAME: &str = "English";

/// One language a person can choose: by the name it gives itself, how far it is translated, and where its file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageChoice {
    pub language: Language,
    pub name: String,
    /// How much of the application it translates, in percent: always 100 for English, which is written in the code.
    pub percent: u8,
    /// Whether this device holds a file of it downloaded with a release.
    pub downloaded: bool,
    /// Whether this device holds a file of it that a person imported.
    pub imported: bool,
    /// Whether the languages branch offers it for this release.
    pub offered: bool,
    /// The size of its file in bytes: of the one this device holds, else of the one offered; zero when not known.
    pub bytes: u64,
    /// Whether its file says a machine translated it: of the one this device holds, else of the one offered.
    pub machine: bool,
}

/// How much of `messages` a file translating `translated` of them covers, in whole percent, never past 100.
pub fn translated_percent(translated: u32, messages: u32) -> u8 {
    if messages == 0 {
        return 0;
    }
    let percent = (u64::from(translated) * 100 / u64::from(messages)).min(100);
    u8::try_from(percent).unwrap_or(100)
}

/// Every language a person can choose here: English first, then each held or offered language once, by name. A language
/// both held and offered counts what the most complete of its files translates.
pub fn language_choices(
    state: &peersus_core::model::LanguageState,
    messages: u32,
) -> Vec<LanguageChoice> {
    let mut choices: Vec<LanguageChoice> = Vec::new();
    let mut add = |file: &peersus_core::model::LanguageFile,
                   downloaded: bool,
                   imported: bool,
                   offered: bool| {
        let percent = translated_percent(file.translated, messages);
        match choices
            .iter_mut()
            .find(|choice| choice.language == file.language)
        {
            Some(choice) => {
                choice.percent = choice.percent.max(percent);
                choice.downloaded |= downloaded;
                choice.imported |= imported;
                choice.offered |= offered;
                if choice.name.is_empty() {
                    file.name.clone_into(&mut choice.name);
                }
                // The held file's size and word are what it takes here; the offered one's only says what a download would bring.
                if choice.bytes == 0 {
                    choice.bytes = file.bytes;
                }
            }
            None => choices.push(LanguageChoice {
                language: file.language,
                name: file.name.clone(),
                percent,
                downloaded,
                imported,
                offered,
                bytes: file.bytes,
                machine: file.machine,
            }),
        }
    };
    // Held files first, so that their sizes are the ones kept.
    for held in &state.held {
        let imported = held.release.is_none();
        add(&held.file, !imported, imported, false);
    }
    for file in &state.offered {
        add(file, false, false, true);
    }
    choices.retain(|choice| !choice.language.is_english());
    for choice in &mut choices {
        if choice.name.trim().is_empty() {
            choice.name = choice.language.tag().to_owned();
        }
    }
    choices.sort_by(|left, right| {
        (left.name.to_lowercase(), left.language).cmp(&(right.name.to_lowercase(), right.language))
    });
    choices.insert(
        0,
        LanguageChoice {
            language: Language::ENGLISH,
            name: ENGLISH_NAME.to_owned(),
            percent: 100,
            downloaded: false,
            imported: false,
            offered: false,
            bytes: 0,
            machine: false,
        },
    );
    choices
}

/// The language a person wants: the one chosen, else the device's, else English.
pub fn wanted_language(
    chosen: Option<Language>,
    state: &peersus_core::model::LanguageState,
) -> Language {
    peersus_app::languages::person_language(chosen, state.device)
}

/// The choice that stands for `language` in `choices`: its own, else its primary language's (a device's `pt-BR` finds `pt`).
pub fn choice_of(choices: &[LanguageChoice], language: Language) -> Option<&LanguageChoice> {
    choices
        .iter()
        .find(|choice| choice.language == language)
        .or_else(|| {
            choices
                .iter()
                .find(|choice| choice.language == language.primary())
        })
}

/// One row of the Binds page: something a key can run, and what is bound to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindRow {
    pub target: peersus_core::bindings::BindTarget,
    /// What it does: an English message id for the application's own, the extension's words for its actions.
    pub label: String,
    /// What it does exactly, shown under the label: an English message id for the application's own, the extension's own words
    /// (`Action::help`) for its actions, none when the extension wrote none.
    pub help: Option<String>,
    /// The extension it belongs to, by the name it is installed under; none for the application's own.
    pub from: Option<String>,
    /// The device it runs for, by its name, for an action that needs one.
    pub device: Option<String>,
    /// The chords bound to it now, up to `bindings::MAX_PER_TARGET`, in the map's order.
    pub chords: Vec<peersus_api::chord::Chord>,
}

impl BindRow {
    /// Whether it has as many chords as it may.
    pub fn full(&self) -> bool {
        self.chords.len() >= peersus_core::bindings::MAX_PER_TARGET
    }
}

/// The rows of the Binds page: the application's own actions, then every action of every installed extension that a key can run,
/// one row per paired device for an action that needs one. Nothing here is named in code: it is read from what is installed.
pub fn bind_rows(view: &View) -> Vec<BindRow> {
    use peersus_core::bindings::{AppAction, BindTarget};
    let bindings = &view.settings.bindings;
    let chords_of = |target: &BindTarget| {
        peersus_core::bindings::chords_of(bindings, target).collect::<Vec<_>>()
    };
    let mut rows: Vec<BindRow> = AppAction::ALL
        .into_iter()
        .map(|action| {
            let target = BindTarget::App { action };
            BindRow {
                chords: chords_of(&target),
                target,
                label: action.label().to_owned(),
                help: Some(action.help().to_owned()),
                from: None,
                device: None,
            }
        })
        .collect();
    for plugin in &view.plugins {
        let name = plugin.package.as_ref().map_or_else(
            || plugin.id.as_str().to_owned(),
            |package| package.name.clone(),
        );
        for action in plugin
            .manifest
            .contributions
            .actions
            .iter()
            .filter(|action| action.bindable())
        {
            let devices: Vec<Option<&Device>> = if action.needs_device {
                view.devices.iter().map(Some).collect()
            } else {
                vec![None]
            };
            for device in devices {
                let target = BindTarget::Plugin {
                    plugin: plugin.id.clone(),
                    action: action.id.clone(),
                    peer: device.map(|device| peersus_core::model::PeerId(device.id.clone())),
                };
                let chords = chords_of(&target);
                rows.push(BindRow {
                    chords,
                    target,
                    label: action.label.clone(),
                    help: action.help.clone(),
                    from: Some(name.clone()),
                    device: device.map(|device| device.name.clone()),
                });
            }
        }
    }
    rows
}

/// Why the page did not bind a chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindRefusal {
    /// The chord already runs what this row (of the page's) says.
    Taken(Box<BindRow>),
    /// The row has as many chords as it may.
    Full,
}

/// The bindings after `chord` is added to `target`'s, or why not: the row (of `rows`, the page's) that holds that chord already,
/// or the target having as many as it may.
pub fn rebound(
    bindings: &peersus_core::bindings::Bindings,
    rows: &[BindRow],
    chord: peersus_api::chord::Chord,
    target: &peersus_core::bindings::BindTarget,
) -> Result<peersus_core::bindings::Bindings, BindRefusal> {
    use peersus_core::bindings::Refused;
    let mut bindings = bindings.clone();
    peersus_core::bindings::bind(&mut bindings, chord, target.clone()).map_err(|refused| {
        match refused {
            Refused::Full => BindRefusal::Full,
            Refused::Taken(held) => BindRefusal::Taken(Box::new(
                rows.iter()
                    .find(|row| row.target == held)
                    .cloned()
                    .unwrap_or(BindRow {
                        target: held,
                        label: "something no longer installed".to_owned(),
                        help: None,
                        from: None,
                        device: None,
                        chords: vec![chord],
                    }),
            )),
        }
    })?;
    Ok(bindings)
}

/// The bindings without `chord`; whatever else its action has stays.
pub fn unbound(
    bindings: &peersus_core::bindings::Bindings,
    chord: peersus_api::chord::Chord,
) -> peersus_core::bindings::Bindings {
    let mut bindings = bindings.clone();
    bindings.remove(&chord);
    bindings
}

/// The most the page can be scrolled: its height less the window it is seen through, never below 0.
pub fn scroll_range(content: f32, viewport: f32) -> f32 {
    (content - viewport).max(0.)
}

/// The edges an offer can be announced at, with the word for each, in the order they are offered. A screen that is held in the
/// hand has a top and a bottom to choose from; a desktop has its sides as well.
pub fn notice_places(sides: bool) -> &'static [(peersus_core::config::NoticePlace, &'static str)] {
    use peersus_core::config::NoticePlace;
    const ALL: &[(NoticePlace, &str)] = &[
        (NoticePlace::Top, "Top"),
        (NoticePlace::Bottom, "Bottom"),
        (NoticePlace::Left, "Left"),
        (NoticePlace::Right, "Right"),
    ];
    if sides { ALL } else { &ALL[..2] }
}

/// The edge that is marked as chosen: the saved one, or the bottom when this screen has no such edge, which is where the offer
/// is then drawn. The saved choice is not changed by being shown.
pub fn notice_place_shown(
    saved: peersus_core::config::NoticePlace,
    sides: bool,
) -> peersus_core::config::NoticePlace {
    if notice_places(sides)
        .iter()
        .any(|(place, _)| *place == saved)
    {
        saved
    } else {
        peersus_core::config::NoticePlace::Bottom
    }
}

/// Where the panel of an offer stands on a screen: its top left corner, in the screen's own pixels.
///
/// `screen` is the screen's top left corner and its size; `panel` the panel's size; `margin` what is kept clear between the
/// panel and the edge it stands at. At the top and the bottom it is centred across; at a side it is centred down. A panel
/// larger than the screen, or a margin larger than what is left, never puts it off the screen: it is kept inside, flush with
/// the top left when it does not fit at all.
pub fn panel_origin(
    place: peersus_core::config::NoticePlace,
    screen: ((i32, i32), (u32, u32)),
    panel: (u32, u32),
    margin: u32,
) -> (i32, i32) {
    use peersus_core::config::NoticePlace;
    let ((left, top), (width, height)) = screen;
    // What is left over in each direction: nothing when the panel does not fit.
    let (across, down) = (
        width.saturating_sub(panel.0),
        height.saturating_sub(panel.1),
    );
    let (near_x, near_y) = (margin.min(across), margin.min(down));
    let (x, y) = match place {
        NoticePlace::Top => (across / 2, near_y),
        NoticePlace::Bottom => (across / 2, down - near_y),
        NoticePlace::Left => (near_x, down / 2),
        NoticePlace::Right => (across - near_x, down / 2),
    };
    (
        left.saturating_add_unsigned(x),
        top.saturating_add_unsigned(y),
    )
}

/// `text` as search compares it: lower case, and each Latin letter with a mark as the letter a person types without it, so that
/// "setari" finds "Setări", "jezyk" "Język" and "strasse" "Straße". The words shown are never changed by it.
pub fn folded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'æ' => out.push_str("ae"),
            'ç' | 'ć' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ğ' => out.push('g'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => out.push('i'),
            'ĺ' | 'ľ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'œ' => out.push_str("oe"),
            'ŕ' | 'ř' => out.push('r'),
            'ś' | 'š' | 'ş' | 'ș' => out.push('s'),
            'ß' => out.push_str("ss"),
            'ť' | 'ţ' | 'ț' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            // A combining mark typed or stored apart from its letter.
            '\u{300}'..='\u{36f}' => {}
            other => out.push(other),
        }
    }
    out
}

/// How well `query` matches `text`, or `None` when it does not. Every character of the query must be found in the text, in order and
/// without regard to case or marks ([`folded`]), so that "bkg" finds "Run in the background". A character found at the start of a word or right after the
/// one before it scores more, so that a match that reads as the words scores higher than one scattered across them. An empty query
/// matches nothing: there is no search yet, and a list of everything would not be a result.
pub fn fuzzy_score(query: &str, text: &str) -> Option<u32> {
    let wanted: Vec<char> = folded(query)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let haystack: Vec<char> = folded(text).chars().collect();
    let window = wanted.len().saturating_mul(3).saturating_add(8);
    let mut best = None;
    for start in haystack
        .iter()
        .enumerate()
        .filter_map(|(index, c)| (*c == wanted[0]).then_some(index))
    {
        let end = (start + window).min(haystack.len());
        let (mut at, mut score, mut previous) = (start, 0u32, None);
        let mut matched = true;
        for c in &wanted {
            let Some(found) = haystack[at..end]
                .iter()
                .position(|h| h == c)
                .map(|offset| at + offset)
            else {
                matched = false;
                break;
            };
            let starts_word = found == 0 || !haystack[found - 1].is_alphanumeric();
            score += 1
                + u32::from(starts_word) * 3
                + u32::from(previous.is_some_and(|p| p + 1 == found)) * 2;
            previous = Some(found);
            at = found + 1;
        }
        if matched {
            best = best.max(Some(score));
        }
    }
    best
}

/// The group a reader is in at a scroll position (`scrolled` pixels from the top of the content, 0 or more): the last whose top has been reached,
/// with `lead` pixels of the next one already counting as being in it, so that a title that has just come up under the header is the current one.
/// `None` while nothing has been measured. The last group is also the current one once the content cannot be scrolled further (`at_end`), however
/// short it is: a last group that is shorter than the screen would otherwise never be reached.
pub fn current_group(offsets: &[f32], scrolled: f32, lead: f32, at_end: bool) -> Option<usize> {
    if offsets.is_empty() {
        return None;
    }
    if at_end {
        return Some(offsets.len() - 1);
    }
    Some(
        offsets
            .iter()
            .rposition(|top| *top <= scrolled + lead)
            .unwrap_or(0),
    )
}
