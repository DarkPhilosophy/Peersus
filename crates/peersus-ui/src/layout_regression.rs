//! The layout engine re-measures only what is marked dirty. These tests mark changes with the toolkit's own
//! `mark_layout` (the one `apply_mutations` uses), for the flags its element diff produces, and check that
//! nothing is left overlapping afterwards.
#![cfg(test)]
use freya::{
    prelude::{LayoutDiff, mark_layout},
    torin::{
        prelude::*,
        test_utils::{TestingTree, test_utils},
    },
};

fn fixed(width: f32, height: f32) -> Node {
    Node::from_size_and_direction(
        Size::Pixels(Length::new(width)),
        Size::Pixels(Length::new(height)),
        Direction::Vertical,
    )
}

fn auto(spacing: f32) -> Node {
    Node::from_size_and_direction_and_spacing(
        Size::Fill,
        Size::Inner,
        Direction::Vertical,
        Length::new(spacing),
    )
}

/// A card (1) holding a body (2) that, in one change, gets new spacing (an inner-layout change) AND a new
/// child (5): what an extension card does when it goes from broken to running and its settings appear.
/// Returns the card and the card below it (3), which must have moved down.
fn grown_card_and_next(mark: impl Fn(&mut Torin<usize>)) -> (Area, Area) {
    let (mut layout, mut measurer) = test_utils();
    let mut tree = TestingTree::default();
    let window = Area::new((0., 0.).into(), (400., 800.).into());
    tree.add(0, None, vec![1, 3], fixed(400., 800.));
    tree.add(1, Some(0), vec![2], auto(0.));
    tree.add(2, Some(1), vec![4], auto(0.));
    tree.add(3, Some(0), vec![], fixed(20., 20.));
    tree.add(4, Some(2), vec![], fixed(10., 30.));
    layout.find_best_root(&tree);
    layout.measure(0, window, &mut measurer, &tree);
    assert_eq!(layout.get(&1).unwrap().area.height(), 30.);

    tree.add(2, Some(1), vec![4, 5], auto(12.));
    tree.add(5, Some(2), vec![], fixed(10., 200.));
    mark(&mut layout);
    layout.find_best_root(&tree);
    layout.measure(0, window, &mut measurer, &tree);
    (layout.get(&1).unwrap().area, layout.get(&3).unwrap().area)
}

#[test]
fn an_inner_layout_change_together_with_a_new_child_moves_the_next_card_down() {
    // The flags the toolkit gives: a rect whose spacing changed (its element diff), and a node it added.
    let (card, next) = grown_card_and_next(|layout| {
        mark_layout(layout, 2, LayoutDiff::STYLE | LayoutDiff::INNER_LAYOUT);
        mark_layout(layout, 5, LayoutDiff::all());
    });
    assert_eq!(
        card.height(),
        30. + 12. + 200.,
        "the card did not grow with its body"
    );
    assert!(
        next.min_y() >= card.max_y(),
        "the next card starts at {} inside the card that ends at {}",
        next.min_y(),
        card.max_y()
    );
}

/// Text whose height depends on the width it is given, like a wrapping help text: 600 px of words in lines of 20 px.
struct WrappingText(usize);

impl LayoutMeasurer<usize> for WrappingText {
    fn measure(
        &mut self,
        _: usize,
        _: &Node,
        size: &Size2D,
    ) -> Option<(Size2D, std::rc::Rc<dyn std::any::Any>)> {
        let width = size.width.max(1.);
        let lines = (600. / width).ceil();
        Some((Size2D::new(width, lines * 20.), std::rc::Rc::new(())))
    }
    fn should_hook_measurement(&mut self, node_id: usize) -> bool {
        node_id == self.0
    }
    fn should_measure_inner_children(&mut self, node_id: usize) -> bool {
        node_id != self.0
    }
}

/// The extension page: two columns that share a flex row, a card in each, and in the left card a block holding a
/// wrapping text. Every container must end at or below what it holds, and the card must contain the text.
#[test]
fn cards_in_flex_columns_contain_their_wrapping_text() {
    let mut layout = Torin::<usize>::new();
    let mut measurer = Some(WrappingText(7));
    let mut tree = TestingTree::default();
    let auto_column = |spacing: f32, padding: f32| Node {
        width: Size::Fill,
        height: Size::Inner,
        direction: Direction::Vertical,
        spacing: Length::new(spacing),
        padding: Gaps::new(padding, padding, padding, padding),
        ..Node::default()
    };
    tree.add(0, None, vec![1], fixed(800., 2000.));
    tree.add(
        1,
        Some(0),
        vec![2, 9],
        Node {
            width: Size::Fill,
            height: Size::Inner,
            direction: Direction::Horizontal,
            content: Content::Flex,
            spacing: Length::new(14.),
            ..Node::default()
        },
    );
    let column = Node {
        width: Size::Flex(Length::new(1.)),
        ..auto_column(14., 0.)
    };
    tree.add(2, Some(1), vec![3], column.clone());
    tree.add(3, Some(2), vec![4], auto_column(12., 18.)); // card
    tree.add(4, Some(3), vec![5], auto_column(12., 0.)); // controls
    tree.add(5, Some(4), vec![7], auto_column(5., 0.)); // settings list / row
    tree.add(
        7,
        Some(5),
        vec![],
        Node {
            width: Size::Fill,
            height: Size::Inner,
            ..Node::default()
        },
    ); // help text
    tree.add(9, Some(1), vec![10], column);
    tree.add(10, Some(9), vec![], fixed(10., 40.));
    layout.find_best_root(&tree);
    layout.measure(
        0,
        Area::new((0., 0.).into(), (800., 2000.).into()),
        &mut measurer,
        &tree,
    );
    let area = |id: usize| layout.get(&id).unwrap().area;
    for (outer, inner, names) in [
        (3, 4, "card/controls"),
        (4, 5, "controls/list"),
        (5, 7, "list/text"),
    ] {
        assert!(
            area(outer).max_y() + 0.01 >= area(inner).max_y(),
            "{names}: the container ends at {} above what it holds, which ends at {} (text {}×{})",
            area(outer).max_y(),
            area(inner).max_y(),
            area(7).width(),
            area(7).height()
        );
    }
}
