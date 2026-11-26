use std::any::Any;

use accesskit::{
    Action,
    ActionRequest,
    Affine,
    Node,
    Rect,
    Role,
    TreeId,
    TreeUpdate,
};
use ragnarok::ProcessedEvents;
use rustc_hash::{
    FxHashMap,
    FxHashSet,
};
use torin::prelude::{
    Area,
    CursorPoint,
    LayoutNode,
};

use crate::{
    accessibility::{
        focus_strategy::AccessibilityFocusStrategy,
        focusable::Focusable,
        id::AccessibilityId,
    },
    data::Overflow,
    elements::{
        label::LabelElement,
        paragraph::ParagraphElement,
    },
    events::emittable::EmmitableEvent,
    integration::{
        EventName,
        EventsChunk,
    },
    node_id::NodeId,
    prelude::{
        AccessibilityFocusMovement,
        Color,
        EventType,
        FontSlant,
        TextAlign,
        TextDecoration,
        TouchPhase,
        WheelEventData,
        WheelSource,
    },
    tree::Tree,
};

pub const ACCESSIBILITY_ROOT_ID: AccessibilityId = AccessibilityId(0);

pub struct AccessibilityTree {
    pub map: FxHashMap<AccessibilityId, NodeId>,
    // Current focused Accessibility Node.
    pub focused_id: AccessibilityId,
}

impl Default for AccessibilityTree {
    fn default() -> Self {
        Self::new(ACCESSIBILITY_ROOT_ID)
    }
}

impl AccessibilityTree {
    pub fn new(focused_id: AccessibilityId) -> Self {
        Self {
            focused_id,
            map: FxHashMap::default(),
        }
    }

    pub fn focused_node_id(&self) -> Option<NodeId> {
        self.map.get(&self.focused_id).cloned()
    }

    /// Dispatch an action requested by assistive technology.
    pub fn handle_action(
        &self,
        request: ActionRequest,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    ) -> bool {
        if request.target_tree != TreeId::ROOT {
            return false;
        }
        let Some(node_id) = self.map.get(&request.target_node).copied() else {
            return false;
        };
        let Some(element) = tree.elements.get(&node_id) else {
            return false;
        };
        let mut builder = element.accessibility().builder.clone();
        element.finish_accessibility(&mut builder);
        if builder.is_disabled() {
            return false;
        }
        match request.action {
            Action::Focus => {
                if !element.accessibility().a11y_focusable.is_enabled() {
                    return false;
                }
                tree.accessibility_diff
                    .request_focus(AccessibilityFocusStrategy::Node(request.target_node));
                true
            }
            Action::Blur => {
                if self.focused_id == request.target_node {
                    tree.accessibility_diff
                        .request_focus(AccessibilityFocusStrategy::Node(ACCESSIBILITY_ROOT_ID));
                }
                true
            }
            Action::ScrollIntoView => {
                self.scroll_to(node_id, tree, events_sender);
                true
            }
            Action::Click | Action::Increment | Action::Decrement
                if !tree.focus_input_is_visible(node_id) =>
            {
                false
            }
            _ => events_sender
                .unbounded_send(EventsChunk::Batch(vec![EmmitableEvent {
                    name: EventName::AccessibilityAction,
                    source_event: EventName::AccessibilityAction,
                    node_id,
                    data: EventType::AccessibilityAction(request),
                    bubbles: true,
                }]))
                .is_ok(),
        }
    }

    /// Initialize the Accessibility Tree
    pub fn init(&mut self, tree: &mut Tree, title: &str) -> TreeUpdate {
        let requested_focus = tree.accessibility_diff.requested_focus.take();
        tree.accessibility_diff.clear();

        let mut nodes = vec![];

        tree.traverse_depth(|node_id| {
            let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
            let layout_node = tree.layout.get(&node_id).unwrap();
            let accessibility_node = Self::create_node(node_id, layout_node, tree, title);
            nodes.push((accessibility_state.a11y_id, accessibility_node));
            self.map.insert(accessibility_state.a11y_id, node_id);
        });

        #[cfg(debug_assertions)]
        tracing::info!(
            "Initialized the Accessibility Tree with {} nodes.",
            nodes.len()
        );

        if !self.can_focus(self.focused_id, tree) {
            self.focused_id = ACCESSIBILITY_ROOT_ID;
        }

        if let Some(requested_focus) = requested_focus {
            self.focus_node_with_strategy(requested_focus, tree);
        }

        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(accesskit::TreeInfo::new(ACCESSIBILITY_ROOT_ID)),
            focus: self.focused_id,
        }
    }

    /// Process any pending Accessibility Tree update
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn process_updates(
        &mut self,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
        title: &str,
    ) -> TreeUpdate {
        let updated_nodes = self.prepare_updates(tree, events_sender);
        self.build_update(tree, title, updated_nodes)
    }

    /// Update focus and node mappings without constructing an AccessKit update.
    pub fn prepare_updates(
        &mut self,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    ) -> FxHashSet<NodeId> {
        let requested_focus = tree.accessibility_diff.requested_focus.take();
        let removed_ids = std::mem::take(&mut tree.accessibility_diff.removed);
        let mut added_or_updated_ids =
            std::mem::take(&mut tree.accessibility_diff.added_or_updated);

        #[cfg(debug_assertions)]
        if !removed_ids.is_empty() || !added_or_updated_ids.is_empty() {
            tracing::info!(
                "Updating the Accessibility Tree with {} removals and {} additions/modifications",
                removed_ids.len(),
                added_or_updated_ids.len()
            );
        }

        // Remove all the removed nodes from the update list
        for node_id in removed_ids.keys() {
            added_or_updated_ids.remove(node_id);
            self.map.retain(|_, id| id != node_id);
        }

        // Mark the parent of the removed nodes as updated
        for parent_id in removed_ids.values() {
            if !removed_ids.contains_key(parent_id) {
                added_or_updated_ids.insert(*parent_id);
            }
        }

        // Register the created/updated nodes
        for node_id in added_or_updated_ids.clone() {
            let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
            self.map.insert(accessibility_state.a11y_id, node_id);

            let node_parent_id = tree.parents.get(&node_id).unwrap_or(&NodeId::ROOT);
            added_or_updated_ids.insert(*node_parent_id);
        }

        let has_request_focus = requested_focus.is_some();

        // Return focus to the root when the focused node is removed or disabled.
        if !self.can_focus(self.focused_id, tree) {
            self.focused_id = ACCESSIBILITY_ROOT_ID;
        }

        // Focus the requested node id if there is one
        let asked_node = match requested_focus.as_ref() {
            Some(AccessibilityFocusStrategy::Node(id)) => Some(*id),
            _ => None,
        };
        if let Some(requested_focus) = requested_focus {
            self.focus_node_with_strategy(requested_focus, tree);
        }

        // Only when the focus landed where it was asked for. A request for a node that cannot take the focus leaves it where it was, and the
        // scroll views make such a request on every move of a drag: revealing the node that had the focus before then pulled the page back
        // to the last switch, button or field that was touched, as soon as it was out of view ("a force field" in the page).
        let landed = asked_node.is_none_or(|id| id == self.focused_id);
        if let Some(node_id) = self.focused_node_id()
            && has_request_focus
            && landed
        {
            self.scroll_to(node_id, tree, events_sender);
        }

        added_or_updated_ids
    }

    /// Build an AccessKit update for nodes prepared by `prepare_updates`.
    pub fn build_update(
        &self,
        tree: &Tree,
        title: &str,
        updated_nodes: impl IntoIterator<Item = NodeId>,
    ) -> TreeUpdate {
        let nodes = updated_nodes
            .into_iter()
            .map(|node_id| {
                let accessibility_state = &tree.accessibility_state[&node_id];
                let layout_node = tree.layout.get(&node_id).unwrap();
                let node = Self::create_node(node_id, layout_node, tree, title);
                (accessibility_state.a11y_id, node)
            })
            .collect();

        TreeUpdate {
            tree_id: TreeId::ROOT,
            nodes,
            tree: Some(accesskit::TreeInfo::new(ACCESSIBILITY_ROOT_ID)),
            focus: self.focused_id,
        }
    }

    fn can_focus(&self, id: AccessibilityId, tree: &Tree) -> bool {
        self.map.get(&id).is_some_and(|node_id| {
            id == ACCESSIBILITY_ROOT_ID
                || tree
                    .accessibility_state
                    .get(node_id)
                    .is_some_and(|state| state.a11y_focusable != Focusable::Disabled)
        })
    }

    /// Focus a Node given the strategy.
    pub fn focus_node_with_strategy(
        &mut self,
        strategy: AccessibilityFocusStrategy,
        tree: &mut Tree,
    ) {
        if let AccessibilityFocusStrategy::Node(id) = strategy {
            if self.can_focus(id, tree) {
                self.focused_id = id;
            }
            return;
        }

        let (navigable_nodes, focused_id) = if strategy.mode()
            == Some(AccessibilityFocusMovement::InsideGroup)
        {
            // Get all accessible nodes in the current group
            let mut group_nodes = Vec::new();

            let node_id = self.map.get(&self.focused_id).unwrap();
            let accessibility_state = tree.accessibility_state.get(node_id).unwrap();
            let member_accessibility_id = accessibility_state.a11y_member_of;
            if let Some(member_accessibility_id) = member_accessibility_id {
                group_nodes = tree
                    .accessibility_groups
                    .get(&member_accessibility_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|id| {
                        let node_id = self.map.get(id).unwrap();
                        let accessibility_state = tree.accessibility_state.get(node_id).unwrap();
                        accessibility_state.a11y_focusable == Focusable::Enabled
                    })
                    .collect();
            }
            (group_nodes, self.focused_id)
        } else {
            let mut nodes = Vec::new();

            tree.traverse_depth(|node_id| {
                let accessibility_state = tree.accessibility_state.get(&node_id).unwrap();
                let member_accessibility_id = accessibility_state.a11y_member_of;

                // Exclude nodes that are members of groups except for the parent of the group
                if let Some(member_accessibility_id) = member_accessibility_id
                    && member_accessibility_id != accessibility_state.a11y_id
                {
                    return;
                }
                if accessibility_state.a11y_focusable == Focusable::Enabled {
                    nodes.push(accessibility_state.a11y_id);
                }
            });

            (nodes, self.focused_id)
        };

        let node_index = navigable_nodes
            .iter()
            .position(|accessibility_id| *accessibility_id == focused_id);

        let target_node = match strategy {
            AccessibilityFocusStrategy::Forward(_) => {
                // Find the next Node
                if let Some(node_index) = node_index {
                    if node_index == navigable_nodes.len() - 1 {
                        navigable_nodes.first().cloned()
                    } else {
                        navigable_nodes.get(node_index + 1).cloned()
                    }
                } else {
                    navigable_nodes.first().cloned()
                }
            }
            AccessibilityFocusStrategy::Backward(_) => {
                // Find the previous Node
                if let Some(node_index) = node_index {
                    if node_index == 0 {
                        navigable_nodes.last().cloned()
                    } else {
                        navigable_nodes.get(node_index - 1).cloned()
                    }
                } else {
                    navigable_nodes.last().cloned()
                }
            }
            _ => unreachable!(),
        };

        self.focused_id = target_node.unwrap_or(focused_id);

        #[cfg(debug_assertions)]
        tracing::info!("Focused {:?} node.", self.focused_id);
    }

    /// Send the necessary wheel events to scroll views so that the given focused [NodeId] is visible on screen.
    fn scroll_to(
        &self,
        node_id: NodeId,
        tree: &mut Tree,
        events_sender: &futures_channel::mpsc::UnboundedSender<EventsChunk>,
    ) {
        let Some(effect_state) = tree.effect_state.get(&node_id) else {
            return;
        };
        // A scroll view that has the focus itself is not a field to bring into view: its drag asks for the focus on every move, and revealing
        // it (it is partly under the bar or the keyboard, so never "entirely visible") moved the page under the finger by the hidden part.
        if effect_state.scrollables.last() == Some(&node_id) {
            return;
        }
        let is_field = tree
            .elements
            .get(&node_id)
            .is_some_and(|element| super::ime::is_ime_role(element.accessibility().builder.role()));
        let mut target_node = node_id;
        let focus_viewport = tree.focus_viewport(node_id);
        let mut projected_target = None;
        let mut ancestor = node_id;
        let mut emmitable_events = Vec::new();
        // Iterate over the inherited scrollables from the closes to the farthest
        for closest_scrollable in effect_state.scrollables.iter().rev() {
            // Every scrollable has a target node, the first scrollable target is the focused node that we want to make visible,
            // the rest scrollables will in the other hand just have the previous scrollable as target
            let target_area =
                projected_target.unwrap_or_else(|| tree.layout.get(&target_node).unwrap().area);
            let scrollable_layout_node = tree.layout.get(closest_scrollable).unwrap();
            let viewport = scrollable_layout_node.visible_area();
            let mut scroll_layout = scrollable_layout_node;
            if focus_viewport.is_some() {
                // ScrollView's clipped child shares its root's viewport but carries the
                // document extent. Use the closest matching clip to this scroll root.
                // Percentage layout at fractional scale can round their edges differently.
                let shares_viewport = |bounds: Area| {
                    [
                        bounds.min_x() - viewport.min_x(),
                        bounds.max_x() - viewport.max_x(),
                        bounds.min_y() - viewport.min_y(),
                        bounds.max_y() - viewport.max_y(),
                    ]
                    .into_iter()
                    .all(|delta| delta.abs() < 0.5)
                };
                while ancestor != *closest_scrollable {
                    if tree.effect_state[&ancestor].overflow == Overflow::Clip
                        && let Some(layout) = tree.layout.get(&ancestor)
                        && shares_viewport(layout.visible_area())
                    {
                        scroll_layout = layout;
                    }
                    let Some(parent) = tree.parents.get(&ancestor) else {
                        break;
                    };
                    ancestor = *parent;
                }
            }
            let inset = focus_viewport.and_then(|room| viewport.intersection(&room));

            // A text field the soft keyboard half covers needs revealing entirely; any other node (a switch, a button, a name) that got the
            // focus because it was pressed or the finger was lifted over it is scrolled to only when it is completely out of view, as
            // upstream does: bringing it entirely into view replaced a fling by a small move (the page stopped as if at its end).
            let visible = if is_field {
                effect_state.is_fully_visible(&tree.layout, &target_area)
            } else {
                effect_state.is_visible(&tree.layout, &target_area)
            };
            if !visible || inset.is_some_and(|room| !room.contains_rect(&target_area)) {
                // Move only as far as needed: down until its bottom is inside the viewport, or up until its top is. A node already
                // inside on an axis is left there, so revealing a field above the keyboard does not throw the page about.
                let viewport = inset.unwrap_or(viewport);
                let reveal = |start: f32, end: f32, view_start: f32, view_end: f32| {
                    if end - start > view_end - view_start || start < view_start {
                        view_start - start
                    } else if end > view_end {
                        view_end - end
                    } else {
                        0.
                    }
                };
                // Only along an axis the scroll view can scroll: a row a few pixels wider than a page that scrolls only up and down would
                // ask for a sideways move, and a wheel of any kind stops a fling the scroll view is running even where it moves nothing.
                let extent = scroll_layout.inner_sizes - scroll_layout.area.size;
                let can_scroll_x = extent.width > 0.5;
                let can_scroll_y = extent.height > 0.5;
                let delta_x = if can_scroll_x {
                    reveal(target_area.min_x(), target_area.max_x(), viewport.min_x(), viewport.max_x())
                } else {
                    0.
                };
                let delta_y = if can_scroll_y {
                    reveal(target_area.min_y(), target_area.max_y(), viewport.min_y(), viewport.max_y())
                } else {
                    0.
                };
                let (delta_x, delta_y) = if focus_viewport.is_some() {
                    // Sizes above are device pixels, while scroll offsets and emitted
                    // wheel deltas are logical. Project only movement this scroller can
                    // make, so an outer scroller can reveal whatever is still covered.
                    let scale = tree.layout_scale_factor;
                    let accessibility = tree.elements.get(closest_scrollable).unwrap().accessibility();
                    // Offsets are positive distances from the start (upstream #2587) and a wheel
                    // delta moves the position by its opposite, so this scroller can still take
                    // a delta from `offset - last` (to its end) up to `offset` (back to its start).
                    let limit = |delta: f32, extent: f32, offset: Option<f64>| {
                        let last = (extent / scale).max(0.).floor();
                        let offset = (offset.unwrap_or_default() as f32).clamp(0., last);
                        let delta = (delta / scale).abs().ceil().copysign(delta);
                        delta.clamp(offset - last, offset).trunc()
                    };
                    let x = limit(delta_x, extent.width, accessibility.builder.scroll_x());
                    let y = limit(delta_y, extent.height, accessibility.builder.scroll_y());
                    projected_target = Some(target_area.translate((x * scale, y * scale).into()));
                    (x, y)
                } else {
                    (delta_x, delta_y)
                };
                // A reveal that moves nothing is not sent: a wheel of zero still sets the scroll view to the position it last drew, which
                // undoes what a drag has moved it by since.
                if delta_x == 0. && delta_y == 0. {
                    continue;
                }
                emmitable_events.push(EmmitableEvent {
                    name: EventName::Wheel,
                    source_event: EventName::Wheel,
                    node_id: *closest_scrollable,
                    data: EventType::Wheel(WheelEventData::new(
                        delta_x as f64,
                        delta_y as f64,
                        WheelSource::Custom,
                        TouchPhase::Moved,
                        CursorPoint::default(),
                        CursorPoint::default(),
                    )),
                    bubbles: false,
                });
                // Change the target to the current scrollable, so that the next scrollable makes sure this one is visible
                target_node = *closest_scrollable;
            }
        }
        events_sender
            .unbounded_send(EventsChunk::Processed(ProcessedEvents {
                emmitable_events,
                ..Default::default()
            }))
            .unwrap();
    }

    /// Create an accessibility node
    pub fn create_node(
        node_id: NodeId,
        layout_node: &LayoutNode,
        tree: &Tree,
        title: &str,
    ) -> Node {
        let element = tree.elements.get(&node_id).unwrap();
        let mut accessibility_data = element.accessibility().into_owned();
        element.finish_accessibility(&mut accessibility_data.builder);

        if node_id == NodeId::ROOT {
            accessibility_data.builder.set_role(Role::Window);
            accessibility_data.builder.set_label(title);
        }

        // Set children
        let children = tree
            .children
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|child| tree.accessibility_state.get(&child).unwrap().a11y_id)
            .collect::<Vec<_>>();
        accessibility_data.builder.set_children(children);

        // Set the area
        let area = layout_node.area.to_f64();
        accessibility_data.builder.set_bounds(Rect {
            x0: area.min_x(),
            x1: area.max_x(),
            y0: area.min_y(),
            y1: area.max_y(),
        });

        // Set inner text
        let inner_text = accessibility_data
            .builder
            .value()
            .map(String::from)
            .or_else(|| {
                tree.children.get(&node_id).and_then(|children| {
                    children.iter().find_map(|child| {
                        let child_element = tree.elements.get(child).unwrap();
                        let mut child_builder = child_element.accessibility().builder.clone();
                        child_element.finish_accessibility(&mut child_builder);
                        child_builder.value().map(String::from)
                    })
                })
            });

        if let Some(inner_text) = inner_text {
            // Accesskit derives the name of Role::Label nodes from the value, not the label
            if accessibility_data.builder.role() == Role::Label {
                accessibility_data.builder.set_value(inner_text);
            } else {
                accessibility_data.builder.set_label(inner_text);
            }
        }

        // Set focusable action
        // This will cause assistive technology to offer the user an option
        // to focus the current element if it supports it.
        if accessibility_data.a11y_focusable.is_enabled() {
            accessibility_data.builder.add_action(Action::Focus);
            accessibility_data.builder.add_action(Action::Blur);
        }

        let builder = &mut accessibility_data.builder;

        if let Some(effect_state) = tree.effect_state.get(&node_id) {
            if let Some(rotation) = effect_state.rotation {
                let rotation = (rotation as f64).to_radians();
                let (sin, cos) = rotation.sin_cos();
                builder.set_transform(Affine::new([cos, sin, -sin, cos, 0.0, 0.0]));
            }

            if effect_state.overflow == Overflow::Clip {
                builder.set_clips_children();
            }
        }

        if let Some(background) = element.style().background.as_color() {
            builder.set_background_color(color_to_accesskit(background));
        }

        let element = element.as_ref() as &dyn Any;
        let is_text_element = element.is::<LabelElement>() || element.is::<ParagraphElement>();
        if !is_text_element {
            builder.set_is_line_breaking_object();
        }

        if let Some(text_style) = tree.text_style_state.get(&node_id) {
            if let Some(color) = text_style.color.as_color() {
                builder.set_foreground_color(color_to_accesskit(color));
            }

            builder.set_font_size(f32::from(text_style.font_size));
            builder.set_font_weight(f32::from(text_style.font_weight));
            builder.set_font_family(text_style.font_families.join(", "));

            if matches!(
                text_style.font_slant,
                FontSlant::Italic | FontSlant::Oblique
            ) {
                builder.set_italic();
            }

            builder.set_text_align(match text_style.text_align {
                TextAlign::Center => accesskit::TextAlign::Center,
                TextAlign::Justify => accesskit::TextAlign::Justify,
                // TODO: change the representation of `Start` and `End` once writing modes are supported.
                TextAlign::Left | TextAlign::Start => accesskit::TextAlign::Left,
                TextAlign::Right | TextAlign::End => accesskit::TextAlign::Right,
            });

            // TODO: adjust this once text directions other than left to right are supported.
            builder.set_text_direction(accesskit::TextDirection::LeftToRight);

            let decoration = accesskit::TextDecoration {
                style: accesskit::TextDecorationStyle::Solid,
                color: text_style
                    .color
                    .as_color()
                    .map(color_to_accesskit)
                    .unwrap_or(color_to_accesskit(Color::BLACK)),
            };
            match text_style.text_decoration {
                TextDecoration::Underline => builder.set_underline(decoration),
                TextDecoration::Overline => builder.set_overline(decoration),
                TextDecoration::LineThrough => builder.set_strikethrough(decoration),
                TextDecoration::None => {}
            }
        }

        accessibility_data.builder
    }
}

/// Convert a Freya [Color] into its [accesskit::Color] equivalent.
fn color_to_accesskit(color: Color) -> accesskit::Color {
    accesskit::Color {
        red: color.r(),
        green: color.g(),
        blue: color.b(),
        alpha: color.a(),
    }
}
