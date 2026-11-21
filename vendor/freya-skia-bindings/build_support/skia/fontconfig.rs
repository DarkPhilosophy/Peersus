use std::{fs, io, path::Path};

const BEFORE: &str = "                    if (FcResultNoId == result) {\n                        break;\n                    }";
const AFTER: &str = "                    if (FcResultNoId == result || FcResultNoMatch == result) {\n                        break;\n                    }";
const FACTORY_BEFORE: &str = "    return sk_make_sp<SkFontMgr_fontconfig>(fc, std::move(scanner));";
const FACTORY_AFTER: &str = r#"    if (!fc) {
        fc = FcInitLoadConfigAndFonts();
    }
    {
        FCLocker lock;
        for (FcSetName set : {FcSetSystem, FcSetApplication}) {
            FcFontSet* fonts = FcConfigGetFonts(fc, set);
            if (!fonts) {
                continue;
            }
            for (int i = fonts->nfont; i-- > 0;) {
                FcChar8* family;
                FcCharSet* charset;
                if (FcPatternGetString(fonts->fonts[i], FC_FAMILY, 0, &family) == FcResultNoMatch &&
                    FcPatternGetCharSet(fonts->fonts[i], FC_CHARSET, 0, &charset) == FcResultNoMatch) {
                    // Fontconfig can merge requested properties into this incomplete pattern,
                    // claiming glyphs its file does not contain. Keep it out of all matching.
                    FcPatternDestroy(fonts->fonts[i]);
                    for (int next = i + 1; next < fonts->nfont; ++next) {
                        fonts->fonts[next - 1] = fonts->fonts[next];
                    }
                    --fonts->nfont;
                }
            }
        }
    }
    return sk_make_sp<SkFontMgr_fontconfig>(fc, std::move(scanner));"#;

// Missing FC_FAMILY returns NoMatch at every index, so waiting only for NoId never ends.
pub fn apply(source: &Path) -> io::Result<()> {
    let path = source.join("src/ports/SkFontMgr_fontconfig.cpp");
    let mut patched = fs::read_to_string(&path)?;
    for (before, after) in [(BEFORE, AFTER), (FACTORY_BEFORE, FACTORY_AFTER)] {
        if patched.matches(after).count() == 1 {
            continue;
        }
        if patched.matches(before).count() != 1 {
            return Err(io::Error::other(
                "unexpected Skia Fontconfig source revision",
            ));
        }
        patched = patched.replacen(before, after, 1);
    }
    if fs::read_to_string(&path)? != patched {
        fs::write(path, patched)?;
    }
    Ok(())
}
