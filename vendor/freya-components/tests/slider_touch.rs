use freya::prelude::*;
use freya_testing::prelude::*;

/// A page that scrolls, with a slider 400 wide whose middle line is 300 from the top, and the value the slider gave last.
#[derive(Clone, Copy)]
struct Page {
    scroll: ScrollController,
    value: State<f64>,
}

const SLIDER_Y: f64 = 310.;

fn page() -> (TestingRunner, Page) {
    let (mut test, page) = TestingRunner::new(
        move || {
            let Page { scroll, mut value } = use_consume::<Page>();
            ScrollView::new_controlled(scroll)
                .child(rect().width(Size::fill()).height(Size::px(300.)))
                .child(
                    Slider::new(move |v| value.set(v))
                        .value(value())
                        .size(Size::px(400.)),
                )
                .child(rect().width(Size::fill()).height(Size::px(1700.)))
        },
        (500., 500.).into(),
        |runner| {
            runner.provide_root_context(|| Page {
                scroll: ScrollController::new(0, 0),
                value: State::create(50.),
            })
        },
        1.,
    );
    test.sync_and_update();
    (test, page)
}

fn position(test: &mut TestingRunner, controller: ScrollController) -> (i32, i32) {
    let position = test.run_in(|| controller.position());
    (position.x as i32, position.y as i32)
}

/// A finger that lands on the slider and goes up scrolls the page, and the slider keeps its value.
#[test]
fn a_touch_that_goes_across_a_slider_scrolls_the_page_and_leaves_the_value() {
    let (mut test, Page { scroll, value }) = page();
    test.press_touch((200., SLIDER_Y));
    for step in 1..=10 {
        // Up, and a little to the side as a finger goes.
        test.move_touch((200. + step as f64 * 0.5, SLIDER_Y - step as f64 * 10.));
        assert_eq!(position(&mut test, scroll), (0, -step * 10));
    }
    // The lift may throw the page on; the value stays where it was.
    test.release_touch((205., SLIDER_Y - 100.));
    assert_eq!(*value.peek(), 50.);
}

/// A finger that lands on the slider and goes along it moves the slider, and the page stays where it is.
#[test]
fn a_touch_that_goes_along_a_slider_moves_it_and_not_the_page() {
    let (mut test, Page { scroll, value }) = page();
    test.press_touch((200., SLIDER_Y));
    for step in 1..=10 {
        test.move_touch((200. + step as f64 * 10., SLIDER_Y - step as f64 * 0.3));
        assert_eq!(position(&mut test, scroll), (0, 0));
    }
    test.release_touch((300., SLIDER_Y - 3.));
    assert!(*value.peek() > 70., "{}", *value.peek());
    assert_eq!(position(&mut test, scroll), (0, 0));
}

/// A tap on the slider puts the value where it was tapped.
#[test]
fn a_tap_on_a_slider_sets_the_value_there() {
    let (mut test, Page { scroll, value }) = page();
    test.press_touch((100., SLIDER_Y));
    test.release_touch((100., SLIDER_Y));
    assert!((15. ..35.).contains(&*value.peek()), "{}", *value.peek());
    assert_eq!(position(&mut test, scroll), (0, 0));
}
