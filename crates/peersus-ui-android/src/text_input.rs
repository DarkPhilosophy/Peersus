//! The one bridge between every text field and the Android input method.
//!
//! A `NativeActivity` has no editor view, so a soft keyboard can only send key events: no composition, no commit of a whole
//! word, no paste from its clipboard panel, no input type. The activity's `TextInputTarget` is that editor view, and this module
//! is its other half. Nothing here knows a field: what the keyboard is told comes from the focused node's accessibility role
//! (which every field gets from its profile), and what the keyboard does arrives as the same key and preedit events a physical
//! keyboard produces, so every field handles it with its existing editing code.
//!
//! NO TEXT IS KEPT OR LOGGED HERE. Commits pass straight through as events; a secret field's text never crosses back to Java.
//!
//! NO PANIC CROSSES. Every native entry point runs inside `catch_unwind`.
#![allow(unsafe_code)]

use freya::prelude::*;
use freya_core::integration::{
    AccessibilityFocusStrategy, ImeEventName, KeyboardEventName, PlatformEvent, UserEvent,
};
use freya_winit::renderer::{NativeEvent, NativeWindowEvent, NativeWindowEventAction};
use freya_winit::{
    integration::is_ime_role,
    plugins::{FreyaPlugin, PluginEvent, PluginHandle},
};
use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::jint,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};
use winit::window::WindowId;

/// The window that receives what the keyboard does. Java calls in on its own thread, so it reaches the window through the
/// renderer's event loop, which is what the handle sends to.
static TARGET: Mutex<Option<(PluginHandle, WindowId)>> = Mutex::new(None);
/// The focused field while it is an editor, so that a keyboard appearing can scroll it back into view.
static FOCUSED_EDITOR: Mutex<Option<AccessibilityId>> = Mutex::new(None);

/// What the keyboard is told about the focused field. The numbers are the contract with `TextInputTarget.java`, which alone
/// knows Android's constants for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
enum Editor {
    None = 0,
    Text = 1,
    Multiline = 2,
    Secret = 3,
    Search = 4,
    Address = 5,
    Email = 6,
    Number = 7,
    Phone = 8,
}

fn editor_for(role: AccessibilityRole) -> Editor {
    match role {
        AccessibilityRole::PasswordInput => Editor::Secret,
        AccessibilityRole::MultilineTextInput | AccessibilityRole::Terminal => Editor::Multiline,
        AccessibilityRole::SearchInput => Editor::Search,
        AccessibilityRole::UrlInput => Editor::Address,
        AccessibilityRole::EmailInput => Editor::Email,
        AccessibilityRole::NumberInput => Editor::Number,
        AccessibilityRole::PhoneNumberInput => Editor::Phone,
        role if is_ime_role(role) => Editor::Text,
        _ => Editor::None,
    }
}

pub struct TextInputPlugin;

impl FreyaPlugin for TextInputPlugin {
    fn plugin_id(&self) -> &'static str {
        "text-input"
    }

    fn on_event(&mut self, event: &mut PluginEvent, handle: PluginHandle) {
        match event {
            PluginEvent::WindowCreated { window, .. } => {
                if let Ok(mut target) = TARGET.lock() {
                    *target = Some((handle, window.id()));
                }
            }
            PluginEvent::WindowClosed { window, .. } => {
                if let Ok(mut target) = TARGET.lock()
                    && target.as_ref().is_some_and(|(_, id)| *id == window.id())
                {
                    *target = None;
                }
            }
            _ => {}
        }
    }

    fn root_component(&self, root: Element) -> Element {
        TextInputRoot { inner: root }.into_element()
    }
}

#[derive(Clone)]
struct TextInputRoot {
    inner: Element,
}

impl PartialEq for TextInputRoot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Tell the activity which editor the keyboard needs; `Editor::None` gives the keyboard up.
fn tell_keyboard(editor: Editor) {
    let told = crate::bridge::with_activity(|env, activity| {
        env.call_method(activity, "setTextEditor", "(I)V", &[(editor as i32).into()])
            .map(drop)
            .map_err(|error| {
                let _ = env.exception_clear();
                error.to_string()
            })
    });
    if told.is_err() {
        tracing::warn!(
            operation = "set-text-editor",
            "Android platform operation failed"
        );
    }
}

impl Component for TextInputRoot {
    fn render(&self) -> impl IntoElement {
        // Tell the keyboard what the focused field is, whenever that changes.
        use_side_effect(move || {
            let window = Platform::get().current_window();
            let editor = editor_for(window.focused_accessibility_node.read().role());
            if let Ok(mut focused) = FOCUSED_EDITOR.lock() {
                *focused =
                    (editor != Editor::None).then(|| *window.focused_accessibility_id.peek());
            }
            tell_keyboard(editor);
        });
        self.inner.clone()
    }
}

/// The keyboard changed how much of the window is visible: scroll the focused field back into view. Focusing it again is the
/// toolkit's own way to do that, and changes nothing else when it already has focus.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TextInputTarget_reveal(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) {
    let _ = catch_unwind(|| {
        let Some(focused) = FOCUSED_EDITOR.lock().ok().and_then(|focused| *focused) else {
            return;
        };
        let Ok(target) = TARGET.lock() else { return };
        let Some((handle, window_id)) = target.as_ref() else {
            return;
        };
        handle.send_event_loop_event(NativeEvent::Window(NativeWindowEvent {
            window_id: *window_id,
            action: NativeWindowEventAction::User(UserEvent::FocusAccessibilityNode(
                AccessibilityFocusStrategy::Node(focused),
            )),
        }));
    });
}

/// What a press on the already focused field does: bring back the keyboard the person closed, as an Android text field does.
/// The activity shows it only if it is hidden, so an open keyboard is never opened a second time.
pub fn pressed_again() -> InputPressedAgain {
    InputPressedAgain(std::rc::Rc::new(|| {
        let window = Platform::get().current_window();
        let editor = editor_for(window.focused_accessibility_node.peek().role());
        if editor != Editor::None {
            tell_keyboard(editor);
        }
    }))
}

fn send(events: impl IntoIterator<Item = PlatformEvent>) {
    let Ok(target) = TARGET.lock() else { return };
    let Some((handle, window)) = target.as_ref() else {
        return;
    };
    for event in events {
        handle.send_platform_event(event, *window);
    }
}

fn press(key: Key, modifiers: Modifiers) -> [PlatformEvent; 2] {
    [KeyboardEventName::KeyDown, KeyboardEventName::KeyUp].map(|name| PlatformEvent::Keyboard {
        name,
        key: key.clone(),
        code: Code::Unidentified,
        modifiers,
    })
}

fn preedit(text: String) -> PlatformEvent {
    PlatformEvent::ImePreedit {
        name: ImeEventName::Preedit,
        cursor: None,
        text,
    }
}

fn string(env: &mut JNIEnv<'_>, text: &JString<'_>) -> Option<String> {
    env.get_string(text).ok().map(Into::into)
}

/// The keyboard's text in composition (a word being typed); empty ends it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TextInputTarget_compose(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    text: JString<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(text) = string(&mut env, &text) {
            send([preedit(text)]);
        }
    }));
}

/// Text the keyboard commits: a typed character, a finished word, a paste from its clipboard panel.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TextInputTarget_commit(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    text: JString<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(text) = string(&mut env, &text).filter(|text| !text.is_empty()) {
            let mut events = vec![preedit(String::new())];
            events.extend(press(Key::Character(text.into()), Modifiers::empty()));
            send(events);
        }
    }));
}

/// Characters the keyboard removes before and after the cursor.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TextInputTarget_delete(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    before: jint,
    after: jint,
) {
    let _ = catch_unwind(|| {
        let backspaces = (0..before.max(0))
            .flat_map(|_| press(Key::Named(NamedKey::Backspace), Modifiers::empty()));
        let deletes =
            (0..after.max(0)).flat_map(|_| press(Key::Named(NamedKey::Delete), Modifiers::empty()));
        send(backspaces.chain(deletes).collect::<Vec<_>>());
    });
}

/// The keyboard's action key (done, search, go) and its editing menu. The numbers are `TextInputTarget`'s.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TextInputTarget_action(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    action: jint,
) {
    let _ = catch_unwind(|| {
        let shortcut = |letter: &str| press(Key::Character(letter.into()), Modifiers::CONTROL);
        let events = match action {
            0 => press(Key::Named(NamedKey::Enter), Modifiers::empty()),
            1 => shortcut("a"),
            2 => shortcut("c"),
            3 => shortcut("x"),
            4 => shortcut("v"),
            _ => return,
        };
        send(events);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_editing_role_gets_an_editor_and_nothing_else_does() {
        assert_eq!(editor_for(AccessibilityRole::PasswordInput), Editor::Secret);
        assert_eq!(editor_for(AccessibilityRole::TextInput), Editor::Text);
        assert_eq!(editor_for(AccessibilityRole::DateInput), Editor::Text);
        assert_eq!(editor_for(AccessibilityRole::Button), Editor::None);
        assert_eq!(editor_for(AccessibilityRole::Unknown), Editor::None);
    }
}
