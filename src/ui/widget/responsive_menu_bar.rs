// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from libcosmic `src/widget/responsive_menu_bar.rs` (rev `d9431dc`).

use std::collections::HashMap;

use crate::ui::Apply;

use crate::ui::Element;
use crate::ui::shell::Core;
use crate::ui::widget::{button, icon, responsive_container};

use crate::ui::widget::menu::{self, ItemHeight, ItemWidth};

#[must_use]
pub fn responsive_menu_bar() -> ResponsiveMenuBar {
    ResponsiveMenuBar::default()
}

pub struct ResponsiveMenuBar {
    collapsed_item_width: ItemWidth,
    item_width: ItemWidth,
    item_height: ItemHeight,
    spacing: f32,
}

impl Default for ResponsiveMenuBar {
    fn default() -> ResponsiveMenuBar {
        ResponsiveMenuBar {
            collapsed_item_width: {
                if matches!(
                    crate::ui::shell::runner::windowing_system(),
                    Some(crate::ui::shell::runner::WindowingSystem::Wayland)
                ) {
                    ItemWidth::Static(150)
                } else {
                    ItemWidth::Static(84)
                }
            },
            item_width: ItemWidth::Uniform(150),
            item_height: ItemHeight::Uniform(30),
            spacing: 0.,
        }
    }
}

impl ResponsiveMenuBar {
    /// Set the item width
    #[must_use]
    pub fn item_width(mut self, item_width: ItemWidth) -> Self {
        self.item_width = item_width;
        self
    }

    /// Set the item height
    #[must_use]
    pub fn item_height(mut self, item_height: ItemHeight) -> Self {
        self.item_height = item_height;
        self
    }

    /// Set the spacing
    #[must_use]
    pub fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }

    /// # Panics
    ///
    /// Will panic if the menu bar collapses without tracking the size
    pub fn into_element<
        'a,
        Message: Clone + 'static,
        A: menu::Action<Message = Message> + Clone,
        S: Into<std::borrow::Cow<'static, str>> + 'static,
    >(
        self,
        core: &Core,
        key_binds: &HashMap<menu::KeyBind, A>,
        // The name rather than the `Id`: `iced_core::widget::Id` has no
        // `Display` and no way to read its string back, and the two derived
        // ids below need the string, so it is passed in and the `Id` is built
        // from it here. `Id` compares by that string, so rebuilding it per
        // frame still matches the `core.menu_bars` key.
        id_name: &'static str,
        action_message: impl Fn(crate::ui::surface::Action<Message>) -> Message
        + Send
        + Sync
        + Clone
        + 'static,
        trees: Vec<(S, Vec<menu::Item<A, S>>)>,
    ) -> Element<'a, Message> {
        use crate::ui::widget::id_container;

        let id = crate::ui::widget::Id::new(id_name);
        let menu_bar_size = core.menu_bars.get(&id);

        #[allow(clippy::if_not_else)]
        if !menu_bar_size.is_some_and(|(limits, size)| {
            let max_size = limits.max();
            max_size.width < size.width
        }) {
            responsive_container::responsive_container(
                id_container(
                    menu::bar(
                        trees
                            .into_iter()
                            .map(|mt: (S, Vec<menu::Item<A, S>>)| {
                                menu::Tree::<_>::with_children(
                                    crate::ui::widget::RcElementWrapper::new(Element::from(
                                        menu::root(mt.0),
                                    )),
                                    menu::items(key_binds, mt.1),
                                )
                            })
                            .collect(),
                    )
                    .item_width(self.item_width)
                    .item_height(self.item_height)
                    .spacing(self.spacing)
                    .on_surface_action(action_message.clone())
                    .window_id_maybe(core.main_window_id()),
                    crate::ui::widget::Id::from(format!("menu_bar_expanded_{id_name}")),
                ),
                id,
                action_message,
            )
            .apply(Element::from)
        } else {
            responsive_container::responsive_container(
                id_container(
                    menu::bar(vec![menu::Tree::<_>::with_children(
                        Element::from(
                            button::icon(icon::line::handle(icon::line::MENU))
                                .padding([4, 12])
                                .class(crate::ui::theme::Button::MenuRoot),
                        ),
                        menu::items(
                            key_binds,
                            trees
                                .into_iter()
                                .map(|mt| menu::Item::Folder(mt.0, mt.1))
                                .collect(),
                        )
                        .into_iter()
                        .map(|t| {
                            t.width(match self.item_width {
                                ItemWidth::Uniform(w) | ItemWidth::Static(w) => w,
                            })
                        })
                        .collect(),
                    )])
                    .item_height(self.item_height)
                    .item_width(self.collapsed_item_width)
                    .spacing(self.spacing)
                    .on_surface_action(action_message.clone())
                    .window_id_maybe(core.main_window_id()),
                    crate::ui::widget::Id::from(format!("menu_bar_collapsed_{id_name}")),
                ),
                id,
                action_message,
            )
            .size(menu_bar_size.unwrap().1)
            .apply(Element::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::Element;
    use crate::ui::iced::{Rectangle, Size};
    use crate::ui::iced_core::widget::{Id, Operation};
    use crate::ui::iced_core::{Event, clipboard, mouse, window};
    use crate::ui::iced_runtime::user_interface::{Cache, UserInterface};
    use crate::ui::widget::{icon, menu};

    fn header(core: &'static crate::ui::shell::Core) -> Element<'static, crate::app::Message> {
        let key_binds: &'static _ = Box::leak(Box::new(std::collections::HashMap::new()));
        let trees: Vec<(String, Vec<menu::Item<crate::app::Action, String>>)> =
            ["File", "Edit", "View", "Sort"]
                .into_iter()
                .map(|name| {
                    (
                        name.to_string(),
                        vec![menu::Item::Button(
                            "New tab".to_string(),
                            None,
                            crate::app::Action::TabNew,
                        )],
                    )
                })
                .collect();
        let bar = super::responsive_menu_bar()
            .item_height(menu::ItemHeight::Dynamic(40))
            .item_width(menu::ItemWidth::Uniform(360))
            .spacing(2.0)
            .into_element(core, key_binds, "menu", crate::app::Message::Surface, trees);
        let pill: Element<'_, crate::app::Message> = crate::ui::widget::container(bar)
            .padding(2)
            .class(crate::ui::theme::Container::Pill)
            .into();
        crate::ui::widget::header_bar()
            .focused(true)
            .start(crate::ui::widget::nav_bar_toggle().on_toggle(crate::app::Message::None))
            .start(pill)
            .end(
                crate::ui::widget::button::icon(icon::line::handle(icon::line::SEARCH))
                    .on_press(crate::app::Message::None)
                    .padding(8),
            )
            .on_minimize(crate::app::Message::None)
            .on_maximize(crate::app::Message::None)
            .on_close(crate::app::Message::None)
            .into()
    }

    struct Find(Vec<(Id, Rectangle)>);
    impl Operation for Find {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
            if let Some(id) = id {
                self.0.push((id.clone(), bounds));
            }
        }
    }

    /// The bar is drawn expanded first, finds it does not fit, and folds on
    /// a later frame, keeping the widget tree it had: as in the app.
    #[test]
    fn a_bar_that_folds_lays_its_button_out_whole() {
        let mut core = crate::ui::shell::Core::default();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let mut cache = Cache::default();
        let mut folded = None;
        for _frame in 0..3 {
            let core_ref: &'static _ = Box::leak(Box::new(core.clone()));
            let mut ui = UserInterface::build(
                header(core_ref),
                Size::new(298.0, 47.0),
                cache,
                &mut renderer,
            );
            let mut messages = Vec::new();
            let _ = ui.update(
                &[Event::Window(window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            let mut find = Find(Vec::new());
            ui.operate(&renderer, &mut find);
            folded = find
                .0
                .into_iter()
                .find(|(id, _)| *id == Id::new("menu_bar_collapsed_menu"))
                .map(|(_, bounds)| bounds);
            cache = ui.into_cache();
            for message in messages {
                if let crate::app::Message::Surface(
                    crate::ui::surface::Action::ResponsiveMenuBar {
                        menu_bar,
                        limits,
                        size,
                    },
                ) = message
                {
                    core.menu_bars.insert(menu_bar, (limits, size));
                }
            }
        }
        let folded = folded.expect("the bar folds");
        // 16 of icon in 12 + 12 by 4 + 4 of padding
        assert_eq!((folded.width, folded.height), (40.0, 24.0));
    }
}
