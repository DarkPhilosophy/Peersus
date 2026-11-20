// Exercises the renderer's real Fontconfig manager with an incomplete font pattern.
#include "include/core/SkFontMgr.h"
#include "include/core/SkFontStyle.h"
#include "include/core/SkString.h"
#include "include/core/SkTypeface.h"
#include "include/ports/SkFontMgr_fontconfig.h"
#include "include/ports/SkFontScanner_FreeType.h"
#include <cassert>
#include <string>
#include <vector>

int main(int argc, char** argv) {
    FcConfig* config = FcConfigCreate();
    assert(config && FcConfigBuildFonts(config));
    FcFontSet* fonts = FcConfigGetFonts(config, FcSetSystem);
    assert(fonts);

    // Missing FC_FAMILY returns NoMatch for every index, never NoId.
    assert(FcFontSetAdd(fonts, FcPatternCreate()));
    FcPattern* named = FcPatternCreate();
    assert(FcPatternAddString(named, FC_FAMILY, (const FcChar8*)"Synthetic family"));
    assert(FcPatternAddString(named, FC_FAMILY, (const FcChar8*)"Synthetic alias"));
    assert(FcFontSetAdd(fonts, named));

    auto manager = SkFontMgr_New_FontConfig(config, SkFontScanner_Make_FreeType());
    assert(manager && manager->countFamilies() == 2);
    std::vector<std::string> families;
    for (int i = 0; i < manager->countFamilies(); ++i) {
        SkString family;
        manager->getFamilyName(i, &family);
        families.emplace_back(family.c_str());
    }
    assert((families == std::vector<std::string>{"Synthetic family", "Synthetic alias"}));
    assert(manager->matchFamily("Synthetic family"));
    assert(!manager->matchFamilyStyle("Synthetic family", SkFontStyle()));
    assert(!manager->matchFamilyStyleCharacter(nullptr, SkFontStyle(), nullptr, 0, 0x41));

    assert(argc == 3);
    auto fallbackConfig = FcConfigCreate();
    assert(fallbackConfig && FcConfigBuildFonts(fallbackConfig));
    assert(FcConfigAppFontAddFile(fallbackConfig, (const FcChar8*)argv[1]));
    auto fallbackFonts = FcConfigGetFonts(fallbackConfig, FcSetSystem);
    assert(fallbackFonts);
    auto appFonts = FcConfigGetFonts(fallbackConfig, FcSetApplication);
    assert(appFonts && appFonts->nfont > 0);
    auto namelessWithCharset = FcPatternDuplicate(appFonts->fonts[0]);
    assert(FcPatternDel(namelessWithCharset, FC_FAMILY));
    assert(FcFontSetAdd(fallbackFonts, namelessWithCharset));
    const int retainedSystemFonts = fallbackFonts->nfont;
    auto globalConfig = FcConfigGetCurrent();
    auto globalFonts = FcConfigGetFonts(globalConfig, FcSetSystem);
    const int globalFontCount = globalFonts->nfont;
    auto incomplete = FcPatternCreate();
    assert(FcPatternAddString(incomplete, FC_FILE, (const FcChar8*)argv[2]));
    assert(FcPatternAddInteger(incomplete, FC_INDEX, 0));
    assert(FcFontSetAdd(fallbackFonts, incomplete));
    auto fallback = SkFontMgr_New_FontConfig(fallbackConfig, SkFontScanner_Make_FreeType());
    assert(fallbackFonts->nfont == retainedSystemFonts);
    assert(FcConfigGetCurrent() == globalConfig && globalFonts->nfont == globalFontCount);
    FcCharSet* retainedCharset = nullptr;
    assert(FcPatternGetCharSet(namelessWithCharset, FC_CHARSET, 0, &retainedCharset) == FcResultMatch);
    for (auto character : {0x25be, 0x25b4, 0x2192}) {
        auto face = fallback->matchFamilyStyleCharacter(nullptr, SkFontStyle(), nullptr, 0, character);
        assert(face && face->unicharToGlyph(character));
    }
}
