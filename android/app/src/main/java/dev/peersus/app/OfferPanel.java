package dev.peersus.app;

import android.content.Context;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.widget.CheckBox;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * The panel of an offer of files, drawn over whatever the person is doing, so that the application never has to open. It lists every file
 * of the offer, each with a box, with where it goes and how big it is; the person ticks the ones they want and presses Accept, or Cancel for
 * the rest. It is drawn with the typeface and the colours of the application (the theme the person chose, the one they made as well), which
 * {@link OfferAppearance} keeps, since this is drawn by the system's window manager and cannot read them.
 *
 * It needs the permission to draw over other applications. It opens by itself when an offer comes, and from a touch of the notification of an offer when that is what announced it. It answers
 * through {@link NotificationActions#offerAnswered}, which is how Rust is told, and which works with the window of the application closed.
 */
final class OfferPanel {
    private static View shown;
    // The files the panel has listed since it came up, and those of them that are ticked, by transfer id. A file is ticked when it is first
    // listed and after that only the person changes it: a file that arrived while the panel was up once came unticked (the panel kept the
    // ticks of the files it had and added none), so Accept left it out with nobody having said so.
    private static final Set<String> listed = new HashSet<>();
    private static final Set<String> ticked = new HashSet<>();
    // Whether the panel is what announces the offer now: set when it is asked for and cleared when it is taken away, on the thread that
    // asks, so that what decides between the panel and the notification never waits for the main thread to have drawn it.
    private static volatile boolean up;
    /** The panel is drawn at the top of the screen. */
    static final int TOP = 0;
    /** The panel is drawn at the bottom of the screen: where it is until the person says otherwise, as the owner's setting is. */
    static final int BOTTOM = 1;
    private static volatile int place = BOTTOM;

    static boolean up() { return up; }

    /** Where the panel is drawn, from the person's setting. Anything that is not the top is the bottom. Returns whether it changed. */
    static boolean place(int value) {
        int next = value == TOP ? TOP : BOTTOM;
        if (place == next) return false;
        place = next;
        return true;
    }

    private OfferPanel() { }

    /** One file of the offer: the transfer it is, where it goes, how big it is. */
    private static final class Line {
        final String id, place, size;
        Line(String id, String place, String size) { this.id = id; this.place = place; this.size = size; }
    }

    /** The files of the offer, from the lines Rust gives: id, place, size, tab separated, one file to a line. */
    private static List<Line> parse(String lines) {
        List<Line> files = new ArrayList<>();
        if (lines == null || lines.isEmpty()) return files;
        for (String text : lines.split("\n")) {
            String[] parts = text.split("\t", 3);
            if (parts.length == 3 && !parts[0].isEmpty()) files.add(new Line(parts[0], parts[1], parts[2]));
        }
        return files;
    }

    /** The files of the offer as words, one to a line (where it goes, how big it is), for what announces the offer without a panel. */
    static String listed(String lines) {
        StringBuilder words = new StringBuilder();
        for (Line line : parse(lines)) {
            if (words.length() > 0) words.append('\n');
            words.append(line.place).append(" · ").append(line.size);
        }
        return words.toString();
    }

    /** Draw the panel again for what is offered now, but only if it is shown. */
    static void refresh(Context context, String offer, String lines, int count) {
        android.os.Handler main = new android.os.Handler(android.os.Looper.getMainLooper());
        main.post(() -> {
            if (shown == null) return;
            Context application = context.getApplicationContext();
            draw(application, offer, lines, AppLanguage.context(application).getResources().getQuantityString(R.plurals.requests_title, count));
        });
    }
    /** Show the panel for the offer, or draw it again when what is offered changed. Safe from any thread. */
    static void show(Context context, String offer, String lines, String title) {
        up = true;
        android.os.Handler main = new android.os.Handler(android.os.Looper.getMainLooper());
        main.post(() -> draw(context.getApplicationContext(), offer, lines, title));
    }

    /** Take the panel away. Safe from any thread, and when none is shown. */
    static void hide(Context context) {
        up = false;
        android.os.Handler main = new android.os.Handler(android.os.Looper.getMainLooper());
        main.post(() -> remove(context.getApplicationContext()));
    }

    /** The panel went by itself (answered, or refused by the system): it announces nothing any more. */
    private static void gone(Context context) {
        up = false;
        remove(context);
    }

    private static void remove(Context context) {
        // Forgotten whether or not a window is up: a panel the system refused leaves no ticks behind for the next one to find.
        listed.clear();
        ticked.clear();
        if (shown == null) return;
        try {
            context.getSystemService(WindowManager.class).removeView(shown);
        } catch (RuntimeException gone) {
            // The window was gone already: nothing to take away.
        }
        shown = null;
    }

    /** The height of one of the system's bars, by the name the system keeps it under, or nothing when it has none. */
    private static int bar(Context context, String name) {
        int id = context.getResources().getIdentifier(name, "dimen", "android");
        return id == 0 ? 0 : context.getResources().getDimensionPixelSize(id);
    }

    private static void draw(Context context, String offer, String lines, String title) {
        // Taken away between being asked for and this turn of the main thread: nothing is drawn, so that what is shown and `up` never disagree.
        if (!up) return;
        if (!android.provider.Settings.canDrawOverlays(context)) {
            gone(context);
            TransferNotifications.panelRefused(context);
            return;
        }
        List<Line> files = parse(lines);
        // Called with the panel shown it is the same question with what changed, and the person's ticks are kept; otherwise it is a new one.
        Set<String> still = new HashSet<>();
        for (Line line : files) {
            still.add(line.id);
            if (listed.add(line.id)) ticked.add(line.id);
        }
        listed.retainAll(still);
        ticked.retainAll(still);
        Set<String> wereListed = new HashSet<>(listed);
        Set<String> wereTicked = new HashSet<>(ticked);
        int[] c = OfferAppearance.kept(context);
        if (c == null) c = new int[] {0xFF0C0C0C, 0xFF141414, 0xFF262626, 0xFFEEF2EE, 0xFF8B9997, 0xFF5A9CE0, 0xFFE06A6A, 0xFFF2F5F0, 0xFF000000};
        // Taking the old window away forgets the ticks, which is right when the panel goes and wrong here, where it is drawn again.
        remove(context);
        listed.addAll(wereListed);
        ticked.addAll(wereTicked);
        View panel = build(context, offer, files, title, c);
        WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE | WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
                PixelFormat.TRANSLUCENT);
        // At the edge the person chose, clear of the system's bar there.
        boolean top = place == TOP;
        params.gravity = top ? Gravity.TOP : Gravity.BOTTOM;
        params.y = top ? dp(context, 24) : bar(context, "navigation_bar_height") + dp(context, 12);
        try {
            context.getSystemService(WindowManager.class).addView(panel, params);
            shown = panel;
        } catch (RuntimeException refused) {
            // The system refused the window (the permission was taken away meanwhile): the offer is announced by the notification instead.
            android.util.Log.w("Peersus", "The panel of an offer could not be shown");
            gone(context);
            TransferNotifications.panelRefused(context);
        }
    }

    private static int dp(Context context, int value) {
        return Math.round(value * context.getResources().getDisplayMetrics().density);
    }

    private static GradientDrawable shape(Context context, int fill, int stroke, int radius) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(fill);
        drawable.setCornerRadius(dp(context, radius));
        if (stroke != 0) drawable.setStroke(dp(context, 1), stroke);
        return drawable;
    }

    private static TextView text(Context context, String words, int size, int colour, boolean bold, Typeface face) {
        TextView view = new TextView(context);
        view.setText(words);
        view.setTextSize(size);
        view.setTextColor(colour);
        view.setTypeface(face, bold ? Typeface.BOLD : Typeface.NORMAL);
        return view;
    }

    private static View build(Context context, String offer, List<Line> files, String title, int[] c) {
        Context localized = AppLanguage.context(context);
        Typeface face = context.getResources().getFont(R.font.quicksand);
        LinearLayout card = new LinearLayout(context);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setPadding(dp(context, 16), dp(context, 14), dp(context, 16), dp(context, 14));
        card.setBackground(shape(context, c[OfferAppearance.SURFACE], c[OfferAppearance.BORDER], 16));
        card.setElevation(dp(context, 8));
        card.addView(text(context, title, 16, c[OfferAppearance.TEXT], true, face));

        LinearLayout list = new LinearLayout(context);
        list.setOrientation(LinearLayout.VERTICAL);
        TextView total = text(context, "", 13, c[OfferAppearance.MUTED], false, face);
        for (Line line : files) {
            CheckBox box = new CheckBox(context);
            box.setText(line.place + "\n" + line.size);
            box.setTextSize(14);
            box.setTextColor(c[OfferAppearance.TEXT]);
            box.setTypeface(face);
            box.setButtonTintList(android.content.res.ColorStateList.valueOf(c[OfferAppearance.ACCENT]));
            box.setChecked(ticked.contains(line.id));
            box.setOnCheckedChangeListener((button, on) -> {
                if (on) ticked.add(line.id); else ticked.remove(line.id);
                total.setText(count(localized, files));
            });
            list.addView(box);
        }
        total.setText(count(localized, files));
        ScrollView scroll = new ScrollView(context);
        scroll.addView(list);
        LinearLayout.LayoutParams scrollParams = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f);
        // At most a part of the screen, so that a long list scrolls inside the card and the buttons stay in view.
        int most = context.getResources().getDisplayMetrics().heightPixels / 2;
        LinearLayout.LayoutParams boxed = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, Math.min(most, dp(context, 56) * Math.max(1, files.size())));
        boxed.topMargin = dp(context, 8);
        card.addView(scroll, boxed);
        card.addView(total);

        LinearLayout buttons = new LinearLayout(context);
        buttons.setOrientation(LinearLayout.HORIZONTAL);
        TextView cancel = button(context, localized.getString(R.string.offer_cancel), c[OfferAppearance.TEXT], c[OfferAppearance.RAISED], c[OfferAppearance.BORDER], face);
        TextView accept = button(context, localized.getString(R.string.offer_accept), c[OfferAppearance.BUTTON_TEXT], c[OfferAppearance.BUTTON], 0, face);
        // Cancel on the left, Accept on the right, where the right hand reaches.
        buttons.addView(cancel, new LinearLayout.LayoutParams(0, dp(context, 44), 1f));
        LinearLayout.LayoutParams acceptParams = new LinearLayout.LayoutParams(0, dp(context, 44), 1f);
        acceptParams.leftMargin = dp(context, 10);
        buttons.addView(accept, acceptParams);
        LinearLayout.LayoutParams row = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        row.topMargin = dp(context, 10);
        card.addView(buttons, row);

        // What was not ticked is refused and what was ticked is accepted, each by its own transfer: the files of an offer are transfers, and
        // the owner answers each one. Whether a window is needed is the owner's to say: with saving on their own on and a folder chosen the
        // files are received with no window at all, and otherwise the owner brings the window forward, for the system's picker of the folder.
        cancel.setOnClickListener(view -> {
            for (Line line : files) NotificationActions.offerAnswered("cancel", line.id);
            gone(context);
        });
        accept.setOnClickListener(view -> {
            for (Line line : files) {
                NotificationActions.offerAnswered(ticked.contains(line.id) ? "accept" : "cancel", line.id);
            }
            gone(context);
        });

        LinearLayout frame = new LinearLayout(context);
        frame.setPadding(dp(context, 12), 0, dp(context, 12), 0);
        frame.addView(card, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        return frame;
    }

    private static String count(Context localized, List<Line> files) {
        int on = 0;
        for (Line line : files) if (ticked.contains(line.id)) on++;
        return on + " / " + files.size();
    }

    private static TextView button(Context context, String words, int textColour, int fill, int stroke, Typeface face) {
        TextView view = text(context, words, 14, textColour, true, face);
        view.setGravity(Gravity.CENTER);
        view.setBackground(shape(context, fill, stroke, 12));
        return view;
    }
}
