use freya::prelude::*;
use freya_testing::prelude::*;

const CONTROL: AccessibilityId = AccessibilityId(100);

fn control() -> Rect {
    let mut presses = use_consume::<State<usize>>();
    rect()
        .width(Size::px(90.))
        .height(Size::px(24.))
        .a11y_id(CONTROL)
        .a11y_focusable(true)
        .a11y_role(AccessibilityRole::Button)
        .on_press(move |_| *presses.write() += 1)
}

fn assistive_click(test: &mut TestingRunner) -> bool {
    let target_tree = test.sync_and_update().tree_id;
    test.send_accessibility_action(AccessibilityActionEventData {
        action: AccessibilityAction::Click,
        target_tree,
        target_node: CONTROL,
        data: None,
    })
}

#[test]
fn focus_viewport_reveals_a_covered_control_before_activation_without_changing_the_default() {
    const SCALE: f64 = 1.5;
    const HEIGHT: f32 = 360.;
    const INSET: f32 = 72.;
    let clear = f64::from(HEIGHT) / SCALE - f64::from(INSET);
    for inset in [None, Some((0., 0., INSET, 0.))] {
        let (mut test, presses) = TestingRunner::new(
            move || {
                rect()
                    .expanded()
                    .map(inset, |el, inset| el.focus_viewport_insets(inset))
                    .child(
                        ScrollView::new()
                            .expanded()
                            .child(rect().height(Size::px(194.)))
                            .child(control())
                            .child(rect().height(Size::px(200.))),
                    )
            },
            (480., HEIGHT).into(),
            |test| test.provide_root_context(|| State::create(0_usize)),
            SCALE,
        );
        test.poll_n(std::time::Duration::ZERO, 5);
        let bottom = |test: &TestingRunner| {
            test.find(|node, element| {
                (element.accessibility().a11y_id == Some(CONTROL))
                    .then(|| f64::from(node.layout().area.max_y()) / SCALE)
            })
            .expect("the control is laid out")
        };
        let before = bottom(&test);
        assert!(before > clear, "fixture is already clear of the inset");
        test.run_in(|| CONTROL.request_focus());
        test.poll_n(std::time::Duration::ZERO, 8);
        assert_eq!(test.sync_and_update().focus, CONTROL);
        let after = bottom(&test);
        if inset.is_some() {
            assert!(after <= clear, "focus did not clear the inset ({clear}): {after}");
        } else {
            assert_eq!(
                after, before,
                "the default viewport moved an exposed control"
            );
        }
        test.press_key(Key::Named(NamedKey::Enter));
        assert!(assistive_click(&mut test));
        test.sync_and_update();
        assert_eq!(*presses.peek(), 2);
    }
}

#[test]
fn focus_viewport_reveals_a_control_under_a_top_inset_on_a_page_scrolled_down() {
    const SCALE: f64 = 1.5;
    const HEIGHT: f32 = 360.;
    const INSET: f32 = 72.;
    let (mut test, _presses) = TestingRunner::new(
        || {
            rect()
                .expanded()
                .focus_viewport_insets((INSET, 0., 0., 0.))
                .child(
                    ScrollView::new()
                        .expanded()
                        .child(rect().height(Size::px(100.)))
                        .child(control())
                        .child(rect().height(Size::px(400.))),
                )
        },
        (480., HEIGHT).into(),
        |test| test.provide_root_context(|| State::create(0_usize)),
        SCALE,
    );
    test.poll_n(std::time::Duration::ZERO, 5);
    let top = |test: &TestingRunner| {
        test.find(|node, element| {
            (element.accessibility().a11y_id == Some(CONTROL))
                .then(|| f64::from(node.layout().area.min_y()) / SCALE)
        })
        .expect("the control is laid out")
    };
    assert!(top(&test) >= f64::from(INSET), "fixture starts under the inset");
    // The page moves 60 px down, so the control now lies under the inset and revealing it
    // has to scroll back toward the start of the page.
    test.scroll((100., 100.), (0., -60.));
    test.poll_n(std::time::Duration::ZERO, 5);
    let covered = top(&test);
    assert!(covered < f64::from(INSET), "the wheel left the control clear: {covered}");
    test.run_in(|| CONTROL.request_focus());
    test.poll_n(std::time::Duration::ZERO, 8);
    assert_eq!(test.sync_and_update().focus, CONTROL);
    let revealed = top(&test);
    assert!(
        revealed >= f64::from(INSET),
        "focus left the control under the inset ({covered} -> {revealed})"
    );
}

#[test]
fn focus_viewport_refuses_hidden_activation_but_does_not_change_pointer_hits() {
    let (mut test, (mut moved, presses)) = TestingRunner::new(
        || {
            let moved = use_consume::<State<bool>>();
            rect()
                .expanded()
                .maybe(moved(), |el| el.focus_viewport_insets((72., 0., 0., 0.)))
                .child(rect().height(Size::px(20.)))
                .child(control())
        },
        (300., 240.).into(),
        |test| {
            let moved = test.provide_root_context(|| State::create(false));
            let presses = test.provide_root_context(|| State::create(0_usize));
            (moved, presses)
        },
        1.,
    );
    test.run_in(|| CONTROL.request_focus());
    assert_eq!(test.sync_and_update().focus, CONTROL);
    test.press_key(Key::Named(NamedKey::Enter));
    assert_eq!(*presses.peek(), 1);

    // Chrome moved over a focused control. This subtree cannot scroll it out.
    moved.set(true);
    test.poll_n(std::time::Duration::ZERO, 4);
    test.press_key(Key::Named(NamedKey::Enter));
    assert!(!assistive_click(&mut test));
    assert_eq!(*presses.peek(), 1);

    // A pointer in a real gap still hits the page: the inset is not another clip.
    test.click_cursor((40., 32.));
    assert_eq!(*presses.peek(), 2);
}
