// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from libcosmic `src/widget/responsive_menu_bar.rs` (rev `d9431dc`).

use std::collections::HashMap;

use apply::Apply;

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
                            button::icon(icon::from_name("open-menu-symbolic"))
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
