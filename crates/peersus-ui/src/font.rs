//! The typeface of the application: Quicksand, embedded in the program so that it looks the same
//! on every machine and needs nothing installed.
//!
//! It is one variable font file: the toolkit instantiates every weight the file offers (a step of
//! a hundred between the ends of its weight axis), so regular, medium and bold all come from it.
//! The licence travels with it (SIL Open Font Licence 1.1, `assets/fonts/OFL.txt`) and is credited
//! on the About tab.

/// The name the typeface is registered under, and asked for by.
pub const FAMILY: &str = "Quicksand";

/// The font file as it was published, byte for byte.
pub const BYTES: &[u8] = include_bytes!("../assets/fonts/Quicksand[wght].ttf");

/// The licence that came with the font file.
pub const LICENCE: &str = include_str!("../assets/fonts/OFL.txt");

#[cfg(test)]
mod tests {
    use super::*;

    /// The four-letter names of the tables of a TrueType file, read from its table directory.
    fn tables(bytes: &[u8]) -> Vec<[u8; 4]> {
        let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        (0..count)
            .map(|i| {
                let at = 12 + i * 16;
                [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
            })
            .collect()
    }

    #[test]
    fn the_embedded_file_is_a_truetype_font_with_a_weight_axis() {
        // `0x00010000` is the TrueType signature; anything else is not a font the toolkit can load.
        assert_eq!(
            &BYTES[..4],
            &[0x00, 0x01, 0x00, 0x00],
            "not a TrueType file"
        );
        let names = tables(BYTES);
        // `fvar` is the table that holds the variation axes: without it there is one weight only,
        // and every bold title in the application would come out regular.
        assert!(
            names.contains(b"fvar"),
            "no variation axes: the weights would not exist"
        );
        assert!(names.contains(b"glyf"), "no outlines");
    }

    #[test]
    fn the_licence_beside_the_font_is_the_open_font_licence_for_this_family() {
        assert!(
            LICENCE.contains("SIL OPEN FONT LICENSE Version 1.1"),
            "not the OFL 1.1"
        );
        assert!(
            LICENCE.contains("Quicksand"),
            "not the licence of this family"
        );
        // The licence allows use and embedding in software, which is what is done here.
        assert!(
            LICENCE.contains("embed"),
            "the permission to embed is missing from the text"
        );
    }

    #[test]
    fn the_family_name_is_the_one_the_screens_ask_for() {
        assert_eq!(FAMILY, "Quicksand");
    }
}
