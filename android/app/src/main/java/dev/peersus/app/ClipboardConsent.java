package dev.peersus.app;

/** One selected operation owns one immutable payload; invalidation never means clipboard fallback. */
final class ClipboardConsent {
    private long token, until;
    private String text;

    void select(long token, String text, long until) {
        clear();
        if (token <= 0 || text == null) return;
        this.token = token;
        this.text = text;
        this.until = until;
    }

    String take(long requested, long now) {
        if (requested <= 0 || requested != token) return null;
        String selected = valid(requested, now) ? text : null;
        text = null;
        return selected;
    }

    boolean valid(long requested, long now) {
        if (now >= until) { clear(); return false; }
        return requested > 0 && requested == token;
    }

    void clear() { token = until = 0; text = null; }
}
