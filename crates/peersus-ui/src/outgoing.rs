//! The name under which a file picked through the system's own picker is offered.
//!
//! On Android the picker does not give a path. It gives a descriptor of the file, opened by the system, and the
//! file is read from that descriptor where it is: nothing is copied, so a large file is read once and nothing is
//! written first (see `Source::from_open` in `peersus-net`).
//!
//! What is left to decide here is the **name**. A file with no path has no name of its own: the other application
//! says what it is called, and it is not trusted. A name such as `../../x`, `/etc/passwd`, an empty name or one that
//! is only dots would be refused by the rule every offer is judged by, and the person would be told that a file
//! could not be sent for a reason they cannot see. So the name is made acceptable first, and what could not be
//! kept of it is replaced and not guessed at.

/// The longest name given, in bytes. A name over 240 is refused by the offer's own rule; some names are in a script
/// that takes three bytes a letter, so the limit is in bytes and not in letters, and a margin is left under it.
pub const LONGEST_NAME: usize = 200;

/// What a file is named when the system gave no usable name, or one that was nothing but separators and dots.
pub const UNNAMED: &str = "file";

/// A name the offer's rule accepts, made from the one the other application gave.
///
/// Whatever is before the last separator is dropped, so that no path survives. Control characters, and the
/// characters that some file systems refuse, become `_`. A name of nothing, or only dots, is [`UNNAMED`]. A name
/// longer than [`LONGEST_NAME`] is cut, on a character boundary, keeping its end so that the extension stays.
pub fn safe_name(given: &str) -> String {
    let last = given.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .trim();
    if trimmed.is_empty() {
        return UNNAMED.to_owned();
    }
    if trimmed.len() <= LONGEST_NAME {
        return trimmed.to_owned();
    }
    // Cut from the front, on a boundary, so that the end (the extension) is what stays.
    let mut start = trimmed.len() - LONGEST_NAME;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_no_path_whatever_the_other_application_sent() {
        for hostile in [
            "../../x",
            "../x",
            "/etc/passwd",
            "a/b/c.txt",
            r"C:\Windows\system32\x.dll",
            "..\\..\\x",
            "x/..",
        ] {
            let name = safe_name(hostile);
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "{hostile:?} became {name:?}, which has a separator"
            );
            assert!(name != ".." && name != ".", "{hostile:?} became {name:?}");
        }
    }

    #[test]
    fn a_name_that_is_nothing_or_only_dots_is_given_a_plain_one() {
        for nothing in [
            "", " ", "   ", ".", "..", "...", " . ", "/", "\\", "//", "a/", "../..",
        ] {
            assert_eq!(safe_name(nothing), UNNAMED, "{nothing:?}");
        }
    }

    #[test]
    fn what_a_file_system_refuses_is_made_harmless_and_an_ordinary_name_is_kept() {
        assert_eq!(safe_name("holiday photo (1).jpg"), "holiday photo (1).jpg");
        assert_eq!(safe_name("raport final 2026.pdf"), "raport final 2026.pdf");
        assert_eq!(safe_name("a:b*c?d\"e<f>g|h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(safe_name("line\nbreak\t.txt"), "line_break_.txt");
        assert_eq!(safe_name("nul\0byte.txt"), "nul_byte.txt");
        // A name in another script is kept as it is.
        assert_eq!(safe_name("фото-ă-é.png"), "фото-ă-é.png");
    }

    #[test]
    fn a_long_name_is_cut_in_bytes_on_a_character_boundary_and_keeps_its_extension() {
        let long = format!("{}.pdf", "a".repeat(500));
        let name = safe_name(&long);
        assert!(name.len() <= LONGEST_NAME);
        assert!(name.ends_with(".pdf"), "{name}");
        // Three-byte letters: cut by letters this would be over the limit in bytes, and not by bytes it would split one.
        let wide = format!("{}.png", "字".repeat(200));
        let cut = safe_name(&wide);
        assert!(cut.len() <= LONGEST_NAME, "{} bytes", cut.len());
        assert!(cut.ends_with(".png"));
    }
}
