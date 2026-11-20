//! The Transfers page: every transfer there is a record of, searched and filtered.
use super::*;

/// Every transfer there is a record of, found by what is typed and by one filter. Searching and filtering only choose what is listed: they never
/// remove anything, and Clear (which does) is the one of the list below, asking twice, as everywhere else.
#[derive(PartialEq)]
pub(crate) struct TransfersPage {
    pub(crate) link: Link,
    pub(crate) view: View,
    pub(crate) palette: Palette,
    pub(crate) form: view::Form,
}

impl Component for TransfersPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let query = use_state(String::new);
        let mut filter = use_state(view::TransferFilter::default);
        let typed = query.read().clone();
        let found: Vec<Progress> =
            view::find_transfers(&self.view.transfers, &typed, *filter.read())
                .into_iter()
                .cloned()
                .collect();
        // One chip for each filter, the one in use filled. Words only, so that they are translated like any other and need no picture. Each word is
        // ONE line (`max_lines(1)`), as on the plugin buttons: a chip is as wide as its word, and the row, wider than the page, scrolls sideways. A
        // word left free to wrap was squeezed to what was left of the page and broke letter by letter down a chip 170 px tall.
        // The scrollbar of a sideways scroller is drawn over its last 20 px and given no room: the chips leave it that room.
        let mut chips = rect()
            .width(Size::auto())
            .horizontal()
            .spacing(8.)
            .padding(Gaps::new(0., 0., 20., 0.));
        for which in view::TransferFilter::ALL {
            let button = Button::new()
                .on_press(move |_| filter.set(which))
                // The row scrolls, so a chip is as wide as its word rather than capped at what is left of the screen.
                .child(
                    label()
                        .text(i18n::tr(which.label()))
                        .max_lines(1)
                        .max_width(Size::Inner),
                );
            chips = chips.child(if *filter.read() == which {
                Element::from(button.filled())
            } else {
                Element::from(button)
            });
        }
        let nothing = self.view.transfers.is_empty() || found.is_empty();
        let mut page = rect()
            .width(Size::fill())
            .spacing(self.form.page_section_gap())
            .child(section("TRANSFERS", palette))
            .child(
                field::field(field::Profile::SEARCH, query)
                    .placeholder(i18n::tr("Search transfers"))
                    .width(Size::fill()),
            )
            // The chips are one row of buttons, and the scroll view they sit in is as tall as that row (`auto`), and not the toolkit's default of
            // `fill`, which takes all the height that is left in the page and leaves nothing for the list after it.
            .child(
                ScrollView::new()
                    .direction(Direction::Horizontal)
                    .height(Size::auto())
                    .child(chips),
            );
        if nothing {
            page = page.child(
                label()
                    .text(i18n::tr(if self.view.transfers.is_empty() {
                        "Nothing has been sent or received yet."
                    } else {
                        "No transfer matches."
                    }))
                    .color(palette.muted)
                    .font_size(15.),
            );
        } else {
            page = page.child(Transfers {
                link: self.link.clone(),
                items: found,
                palette,
                auto_save: self.view.settings.auto_save,
                save_folder: self.view.settings.save_folder.clone(),
                context: false,
                prepared: Vec::new(),
                actions: None,
            });
        }
        frame(palette, self.form, page)
    }
}
