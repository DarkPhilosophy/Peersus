//! Interface messages; user-controlled names and paths never pass through the catalog.
mod catalog;

use freya::prelude::{State, try_consume_context};
pub use peersus_core::config::Language;

#[derive(Clone, Copy)]
pub(crate) struct Current(pub State<Language>);

pub fn language() -> Language {
    try_consume_context::<Current>()
        .map(|current| current.0())
        .unwrap_or_default()
}

pub fn tr(english: &str) -> &str {
    translate(language(), english)
}

pub fn translate(language: Language, english: &str) -> &str {
    catalog::translate(language, english)
}

/// Whether a text is a message of the catalog, which is the only way to tell one that is missing from one that reads the same in every
/// language.
pub fn has(english: &str) -> bool {
    catalog::has(english)
}

/// Expand the translated template once. Inserted data is never parsed as another placeholder.
pub fn format(language: Language, english: &str, arguments: &[(&str, &str)]) -> String {
    let mut remaining = translate(language, english);
    let mut result = String::with_capacity(remaining.len());
    while let Some(start) = remaining.find('{') {
        result.push_str(&remaining[..start]);
        let tail = &remaining[start + 1..];
        let Some(end) = tail.find('}') else {
            result.push_str(&remaining[start..]);
            return result;
        };
        let key = &tail[..end];
        if let Some((_, value)) = arguments.iter().find(|(name, _)| *name == key) {
            result.push_str(value);
        } else {
            result.push_str(&remaining[start..start + end + 2]);
        }
        remaining = &tail[end + 1..];
    }
    result.push_str(remaining);
    result
}

fn form(language: Language, count: u64) -> usize {
    match language {
        Language::Romanian if count == 1 => 0,
        Language::Romanian if count == 0 || (1..20).contains(&(count % 100)) => 1,
        Language::Romanian => 2,
        Language::Polish if count == 1 => 0,
        Language::Polish
            if (2..=4).contains(&(count % 10)) && !(12..=14).contains(&(count % 100)) =>
        {
            1
        }
        Language::Polish => 2,
        Language::Czech if count == 1 => 0,
        Language::Czech if (2..=4).contains(&count) => 1,
        Language::Czech => 2,
        Language::French if count <= 1 => 0,
        Language::Turkish => 0,
        _ if count == 1 => 0,
        _ => 1,
    }
}

pub fn files(language: Language, count: usize) -> String {
    let nouns = match language {
        Language::English => ["file", "files", "files"],
        Language::Romanian => ["fișier", "fișiere", "de fișiere"],
        Language::German => ["Datei", "Dateien", "Dateien"],
        Language::French => ["fichier", "fichiers", "fichiers"],
        Language::Spanish => ["archivo", "archivos", "archivos"],
        Language::Italian => ["file", "file", "file"],
        Language::Portuguese => ["ficheiro", "ficheiros", "ficheiros"],
        Language::Polish => ["plik", "pliki", "plików"],
        Language::Dutch => ["bestand", "bestanden", "bestanden"],
        Language::Turkish => ["dosya", "dosya", "dosya"],
        Language::Czech => ["soubor", "soubory", "souborů"],
        Language::Swedish => ["fil", "filer", "filer"],
    };
    format!("{count} {}", nouns[form(language, count as u64)])
}

#[derive(Clone, Copy)]
pub enum TimeUnit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

pub fn ago(language: Language, count: u64, unit: TimeUnit) -> String {
    let units = match language {
        Language::English => [
            ["minute", "minutes", "minutes"],
            ["hour", "hours", "hours"],
            ["day", "days", "days"],
            ["month", "months", "months"],
            ["year", "years", "years"],
        ],
        Language::Romanian => [
            ["minut", "minute", "de minute"],
            ["oră", "ore", "de ore"],
            ["zi", "zile", "de zile"],
            ["lună", "luni", "de luni"],
            ["an", "ani", "de ani"],
        ],
        Language::German => [
            ["Minute", "Minuten", "Minuten"],
            ["Stunde", "Stunden", "Stunden"],
            ["Tag", "Tagen", "Tagen"],
            ["Monat", "Monaten", "Monaten"],
            ["Jahr", "Jahren", "Jahren"],
        ],
        Language::French => [
            ["minute", "minutes", "minutes"],
            ["heure", "heures", "heures"],
            ["jour", "jours", "jours"],
            ["mois", "mois", "mois"],
            ["an", "ans", "ans"],
        ],
        Language::Spanish => [
            ["minuto", "minutos", "minutos"],
            ["hora", "horas", "horas"],
            ["día", "días", "días"],
            ["mes", "meses", "meses"],
            ["año", "años", "años"],
        ],
        Language::Italian => [
            ["minuto", "minuti", "minuti"],
            ["ora", "ore", "ore"],
            ["giorno", "giorni", "giorni"],
            ["mese", "mesi", "mesi"],
            ["anno", "anni", "anni"],
        ],
        Language::Portuguese => [
            ["minuto", "minutos", "minutos"],
            ["hora", "horas", "horas"],
            ["dia", "dias", "dias"],
            ["mês", "meses", "meses"],
            ["ano", "anos", "anos"],
        ],
        Language::Polish => [
            ["minutę", "minuty", "minut"],
            ["godzinę", "godziny", "godzin"],
            ["dzień", "dni", "dni"],
            ["miesiąc", "miesiące", "miesięcy"],
            ["rok", "lata", "lat"],
        ],
        Language::Dutch => [
            ["minuut", "minuten", "minuten"],
            ["uur", "uur", "uur"],
            ["dag", "dagen", "dagen"],
            ["maand", "maanden", "maanden"],
            ["jaar", "jaar", "jaar"],
        ],
        Language::Turkish => [
            ["dakika", "dakika", "dakika"],
            ["saat", "saat", "saat"],
            ["gün", "gün", "gün"],
            ["ay", "ay", "ay"],
            ["yıl", "yıl", "yıl"],
        ],
        Language::Czech => [
            ["minutou", "minutami", "minutami"],
            ["hodinou", "hodinami", "hodinami"],
            ["dnem", "dny", "dny"],
            ["měsícem", "měsíci", "měsíci"],
            ["rokem", "lety", "lety"],
        ],
        Language::Swedish => [
            ["minut", "minuter", "minuter"],
            ["timme", "timmar", "timmar"],
            ["dag", "dagar", "dagar"],
            ["månad", "månader", "månader"],
            ["år", "år", "år"],
        ],
    };
    let row = match unit {
        TimeUnit::Minute => 0,
        TimeUnit::Hour => 1,
        TimeUnit::Day => 2,
        TimeUnit::Month => 3,
        TimeUnit::Year => 4,
    };
    let duration = format!("{count} {}", units[row][form(language, count)]);
    format(language, "{duration} ago", &[("duration", &duration)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_forms_follow_the_selected_language() {
        for (count, romanian, polish, czech) in [
            (1, "1 fișier", "1 plik", "1 soubor"),
            (2, "2 fișiere", "2 pliki", "2 soubory"),
            (12, "12 fișiere", "12 plików", "12 souborů"),
            (20, "20 de fișiere", "20 plików", "20 souborů"),
            (22, "22 de fișiere", "22 pliki", "22 souborů"),
            (101, "101 fișiere", "101 plików", "101 souborů"),
        ] {
            assert_eq!(files(Language::Romanian, count), romanian);
            assert_eq!(files(Language::Polish, count), polish);
            assert_eq!(files(Language::Czech, count), czech);
        }
    }

    #[test]
    fn translated_sentences_do_not_reinterpret_device_names() {
        assert_eq!(
            format(
                Language::Romanian,
                "{name} wants to send you a file",
                &[("name", "Send {name} {files}")]
            ),
            "Send {name} {files} vrea să îți trimită un fișier"
        );
        assert_eq!(
            format(
                Language::German,
                "{name} wants to send you a file",
                &[("name", "{name}")]
            ),
            "{name} möchte dir eine Datei senden"
        );
    }

    #[test]
    fn elapsed_time_uses_language_specific_order_and_inflection() {
        assert_eq!(
            ago(Language::Romanian, 20, TimeUnit::Minute),
            "acum 20 de minute"
        );
        assert_eq!(ago(Language::German, 2, TimeUnit::Day), "vor 2 Tagen");
        assert_eq!(
            ago(Language::Polish, 12, TimeUnit::Month),
            "12 miesięcy temu"
        );
        assert_eq!(ago(Language::Czech, 3, TimeUnit::Hour), "před 3 hodinami");
        assert_eq!(ago(Language::Swedish, 1, TimeUnit::Year), "för 1 år sedan");
    }

    /// Every literal the screens hand to the translator must be a catalog message.
    /// Whitespace between a call, its language argument and its message is irrelevant:
    /// rustfmt may put them on separate lines. Comments and test code are not screens.
    #[test]
    fn every_string_the_screens_translate_has_a_row_in_the_catalog() {
        const SOURCES: &[(&str, &str)] = &[
            ("lib.rs", include_str!("lib.rs")),
            ("view.rs", include_str!("view.rs")),
            ("plugin_controls.rs", include_str!("plugin_controls.rs")),
            ("settings_search.rs", include_str!("settings_search.rs")),
            ("about.rs", include_str!("about.rs")),
            ("relay_choice.rs", include_str!("relay_choice.rs")),
            ("accent_picker.rs", include_str!("accent_picker.rs")),
            ("interface_size.rs", include_str!("interface_size.rs")),
            ("language_picker.rs", include_str!("language_picker.rs")),
            (
                "settings_navigation.rs",
                include_str!("settings_navigation.rs"),
            ),
            ("sidebar.rs", include_str!("sidebar.rs")),
            ("support.rs", include_str!("support.rs")),
        ];
        let mut missing = Vec::new();
        let mut scanned = 0;
        for (file, source) in SOURCES {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            let production = production
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for (callee, language) in [
                ("i18n::tr(", None),
                ("translate(", Some("language")),
                ("translate(", Some("i18n::language()")),
                ("format(", Some("i18n::language()")),
                ("format(", Some("language")),
            ] {
                let mut rest = production.as_str();
                while let Some(at) = rest.find(callee) {
                    let after = rest[at + callee.len()..].trim_start();
                    rest = after;
                    let after = if let Some(language) = language {
                        let Some(after) = after.strip_prefix(language) else {
                            continue;
                        };
                        let Some(after) = after.trim_start().strip_prefix(',') else {
                            continue;
                        };
                        after.trim_start()
                    } else {
                        after
                    };
                    let Some(after) = after.strip_prefix('"') else {
                        continue;
                    };
                    let end = (0..after.len()).find(|&i| {
                        after.as_bytes()[i] == b'"' && (i == 0 || after.as_bytes()[i - 1] != b'\\')
                    });
                    if let Some(end) = end {
                        scanned += 1;
                        let id = after[..end].replace("\\\"", "\"").replace("\\\\", "\\");
                        if !catalog::has(&id) {
                            missing.push(format!("{file}: {id}"));
                        }
                    }
                    rest = &after[end.unwrap_or(after.len())..];
                }
            }
        }
        assert!(
            scanned > 100,
            "the scan found too few strings to be reading the sources: {scanned}"
        );
        assert!(
            missing.is_empty(),
            "add these to the catalog in every language: {missing:#?}"
        );
    }
}
