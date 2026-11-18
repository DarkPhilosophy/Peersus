package dev.peersus.app;

import android.content.Context;
import android.text.InputType;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * The editor view a NativeActivity lacks. The interface is drawn by Rust; this invisible view only gives the input method an
 * InputConnection, so that composition, whole-word commits, the keyboard's clipboard panel and its editing menu reach the focused
 * field. It keeps no text: what the keyboard does is passed to Rust at once, and Rust's field edits it like any other key. The kind of
 * editor (see {@link #setEditor}) is the only thing Rust says, decided there from the focused field's role.
 */
final class TextInputTarget extends View {
    // The kinds Rust sends, in text_input.rs.
    private static final int NONE = 0, TEXT = 1, MULTILINE = 2, SECRET = 3, SEARCH = 4, ADDRESS = 5, EMAIL = 6, NUMBER = 7, PHONE = 8;
    // The actions Rust understands, in text_input.rs.
    private static final int ENTER = 0, SELECT_ALL = 1, COPY = 2, CUT = 3, PASTE = 4;

    static native void compose(String text);
    static native void commit(String text);
    static native void action(int action);
    static native void reveal();

    private int keyboardHeight = -1;
    private final Runnable revealOnce = TextInputTarget::reveal;

    /**
     * The keyboard's height as the window insets say it; it changes on every frame of its animation. Once it has stopped changing,
     * the focused field is scrolled into view, once: each reveal scrolls by a relative amount, so repeating it before the interface
     * has laid out again would scroll past the field.
     */
    void keyboardInsetChanged(int height) {
        if (height == keyboardHeight) return;
        keyboardHeight = height;
        removeCallbacks(revealOnce);
        if (height > 0) postDelayed(revealOnce, 250);
    }
    static native void delete(int before, int after);

    private int editor = NONE;

    TextInputTarget(Context context) {
        super(context);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    /** Called on the UI thread. NONE gives the keyboard up; any other kind takes it, restarting input when the kind changed. */
    void setEditor(int kind) {
        InputMethodManager input = getContext().getSystemService(InputMethodManager.class);
        if (kind == NONE) {
            editor = NONE;
            input.hideSoftInputFromWindow(getWindowToken(), 0);
            return;
        }
        boolean changed = kind != editor || !hasFocus();
        editor = kind;
        if (!hasFocus()) requestFocus();
        if (changed) input.restartInput(this);
        input.showSoftInput(this, 0);
    }

    @Override public boolean onCheckIsTextEditor() { return editor != NONE; }

    @Override public InputConnection onCreateInputConnection(EditorInfo info) {
        if (editor == NONE) return null;
        info.inputType = inputType(editor);
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | (editor == SEARCH ? EditorInfo.IME_ACTION_SEARCH : editor == MULTILINE ? EditorInfo.IME_ACTION_NONE : EditorInfo.IME_ACTION_DONE)
                | (editor == SECRET ? EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING : 0);
        return new Connection(this);
    }

    private static int inputType(int kind) {
        switch (kind) {
            case SECRET: return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD;
            case MULTILINE: return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            case ADDRESS: return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI;
            case EMAIL: return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS;
            case NUMBER: return InputType.TYPE_CLASS_NUMBER;
            case PHONE: return InputType.TYPE_CLASS_PHONE;
            default: return InputType.TYPE_CLASS_TEXT;
        }
    }

    /** Forwards each editing call; holds only the word in composition, so that finishing it can commit it. */
    private static final class Connection extends BaseInputConnection {
        private String composing = "";

        Connection(View view) { super(view, false); }

        @Override public boolean setComposingText(CharSequence text, int cursor) {
            composing = text.toString();
            compose(composing);
            return true;
        }

        @Override public boolean commitText(CharSequence text, int cursor) {
            composing = "";
            commit(text.toString());
            return true;
        }

        @Override public boolean finishComposingText() {
            if (!composing.isEmpty()) {
                String finished = composing;
                composing = "";
                commit(finished);
            }
            return true;
        }

        @Override public boolean deleteSurroundingText(int before, int after) {
            delete(before, after);
            return true;
        }

        @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
            delete(before, after);
            return true;
        }

        @Override public boolean performEditorAction(int code) {
            action(ENTER);
            return true;
        }

        @Override public boolean performContextMenuAction(int id) {
            if (id == android.R.id.selectAll) action(SELECT_ALL);
            else if (id == android.R.id.copy) action(COPY);
            else if (id == android.R.id.cut) action(CUT);
            else if (id == android.R.id.paste || id == android.R.id.pasteAsPlainText) action(PASTE);
            else return false;
            return true;
        }

        @Override public boolean sendKeyEvent(KeyEvent event) {
            if (event.getAction() == KeyEvent.ACTION_DOWN) {
                switch (event.getKeyCode()) {
                    case KeyEvent.KEYCODE_DEL: delete(1, 0); return true;
                    case KeyEvent.KEYCODE_FORWARD_DEL: delete(0, 1); return true;
                    case KeyEvent.KEYCODE_ENTER: action(ENTER); return true;
                    default: break;
                }
            } else if (event.getAction() == KeyEvent.ACTION_UP) {
                switch (event.getKeyCode()) {
                    case KeyEvent.KEYCODE_DEL: case KeyEvent.KEYCODE_FORWARD_DEL: case KeyEvent.KEYCODE_ENTER: return true;
                    default: break;
                }
            }
            return super.sendKeyEvent(event);
        }
    }
}
