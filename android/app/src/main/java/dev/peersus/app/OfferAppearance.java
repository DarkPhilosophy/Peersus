package dev.peersus.app;

import android.content.Context;
import android.content.SharedPreferences;

/**
 * The colours of the interface, kept for the panel of an offer. The panel is drawn outside the interface, and it can be made when the
 * interface was never drawn (a file offered while the window is closed), so the colours the interface is drawn with are given here whenever
 * they change and kept in the preferences. Nothing here is a colour of its own: the theme the person chose, and the one they made, is what
 * reaches it.
 */
final class OfferAppearance {
    private static final String PREFERENCES = "appearance";
    private static final String KEY = "colours";

    // Nine colours, as the interface gives them: surface, raised, border, text, muted, accent, bad, button, button text.
    static final int SURFACE = 0, RAISED = 1, BORDER = 2, TEXT = 3, MUTED = 4, ACCENT = 5, BAD = 6, BUTTON = 7, BUTTON_TEXT = 8;

    private OfferAppearance() { }

    /** Keep the colours the interface is drawn with now. Called from Rust whenever they change. */
    static void keep(Context context, int[] colours) {
        if (colours == null || colours.length != 9) return;
        StringBuilder text = new StringBuilder();
        for (int colour : colours) {
            if (text.length() > 0) text.append(',');
            text.append(colour & 0xFFFFFF);
        }
        SharedPreferences preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE);
        if (!text.toString().equals(preferences.getString(KEY, ""))) preferences.edit().putString(KEY, text.toString()).apply();
    }

    /** The colours kept, or null when the interface never gave any: the panel then has the colours it is written with. */
    static int[] kept(Context context) {
        String text = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(KEY, "");
        String[] parts = text.split(",");
        if (parts.length != 9) return null;
        int[] colours = new int[9];
        try {
            for (int i = 0; i < 9; i++) colours[i] = 0xFF000000 | Integer.parseInt(parts[i].trim());
        } catch (NumberFormatException damaged) {
            return null;
        }
        return colours;
    }
}
