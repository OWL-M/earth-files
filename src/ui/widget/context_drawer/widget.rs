// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use super::overlay::Overlay;
use crate::ui::widget::{self, LayerContainer, button, container, icon, scrollable, text};
use apply::Apply;
use iced::Renderer;

use crate::fl;
use crate::ui::{Element, Theme};
use std::borrow::Cow;

use crate::ui::convert::{PushMaybe, ToPadding, ToPixels};
use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Alignment, Clipboard, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse,
    overlay as iced_overlay, renderer,
};

#[must_use]
pub struct ContextDrawer<'a, Message> {
    id: Option<iced_core::widget::Id>,
    content: Element<'a, Message>,
    drawer: Element<'a, Message>,
    on_close: Option<Message>,
}

impl<'a, Message: Clone + 'static> ContextDrawer<'a, Message> {
    pub fn new_inner<Drawer>(
        title: Option<Cow<'a, str>>,
        actions: Option<Element<'a, Message>>,
        header: Option<Element<'a, Message>>,
        footer: Option<Element<'a, Message>>,
        drawer: Drawer,
        on_close: Message,
        max_width: f32,
    ) -> Element<'a, Message>
    where
        Drawer: Into<Element<'a, Message>>,
    {
        Self::new_inner_overlay(
            title, actions, header, footer, drawer, on_close, max_width, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_inner_overlay<Drawer>(
        title: Option<Cow<'a, str>>,
        actions: Option<Element<'a, Message>>,
        header: Option<Element<'a, Message>>,
        footer: Option<Element<'a, Message>>,
        drawer: Drawer,
        on_close: Message,
        max_width: f32,
        overlay: bool,
    ) -> Element<'a, Message>
    where
        Drawer: Into<Element<'a, Message>>,
    {
        #[inline(never)]
        #[allow(clippy::too_many_arguments)]
        fn inner<'a, Message: Clone + 'static>(
            title: Option<Cow<'a, str>>,
            actions_opt: Option<Element<'a, Message>>,
            header_opt: Option<Element<'a, Message>>,
            footer_opt: Option<Element<'a, Message>>,
            drawer: Element<'a, Message>,
            on_close: Message,
            max_width: f32,
            overlay: bool,
        ) -> Element<'a, Message> {
            let crate::ui::theme::Spacing {
                space_xxs,
                space_s,
                space_m,
                space_l,
                ..
            } = crate::ui::theme::spacing();

            let horizontal_padding = if max_width < 392.0 { space_s } else { space_l };

            let (actions_slot, column_title) = if let Some(actions) = actions_opt {
                let actions = actions
                    .apply(container)
                    .width(Length::Fill)
                    .apply(Element::from);
                let title = title.map(|title| text::title4(title).width(Length::Fill));
                (actions, title)
            } else {
                let title = title
                    .map(|title| text::title4(title).width(Length::Fill).apply(Element::from))
                    .unwrap_or_else(|| widget::space::horizontal().apply(Element::from));
                (title, None)
            };

            let header_row = crate::ui::widget::Row::with_capacity(2)
                .push(actions_slot)
                .push(
                    button::text(fl!("close"))
                        .trailing_icon(icon::from_name("go-next-symbolic"))
                        .on_press(on_close),
                );
            let header = crate::ui::widget::Column::with_capacity(3)
                .align_x(Alignment::Center)
                .padding([space_m, horizontal_padding])
                .spacing(space_m.to_pixels())
                .push(header_row)
                .push_maybe(column_title)
                .push_maybe(header_opt);
            let footer = footer_opt.map(|element| {
                container(element)
                    .align_y(Alignment::Center)
                    .padding([space_xxs, horizontal_padding])
            });
            let pane = crate::ui::widget::Column::with_capacity(3)
                .push(header)
                .push(
                    container(drawer)
                        .padding(
                            ([
                                0,
                                horizontal_padding,
                                if footer.is_some() { 0 } else { space_l },
                                horizontal_padding,
                            ])
                            .to_padding(),
                        )
                        .apply(scrollable)
                        .height(Length::Fill),
                )
                .push_maybe(footer);

            // XXX new limits do not exactly handle the max width well for containers
            // XXX this is a hack to get around that
            container(
                LayerContainer::new(pane)
                    .layer(crate::ui::theme::Layer::Primary)
                    .class(crate::ui::theme::Container::ContextDrawer {
                        transparent: !overlay,
                    })
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .max_width(max_width),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .into()
        }

        inner(
            title,
            actions,
            header,
            footer,
            drawer.into(),
            on_close,
            max_width,
            overlay,
        )
    }

    /// Creates an empty [`ContextDrawer`].
    #[allow(clippy::too_many_arguments)]
    pub fn new<Content, Drawer>(
        title: Option<Cow<'a, str>>,
        actions: Option<Element<'a, Message>>,
        header: Option<Element<'a, Message>>,
        footer: Option<Element<'a, Message>>,
        content: Content,
        drawer: Drawer,
        on_close: Message,
        max_width: f32,
    ) -> Self
    where
        Content: Into<Element<'a, Message>>,
        Drawer: Into<Element<'a, Message>>,
    {
        let drawer = Self::new_inner_overlay(
            title, actions, header, footer, drawer, on_close, max_width, true,
        );

        ContextDrawer {
            id: None,
            content: content.into(),
            drawer,
            on_close: None,
        }
    }

    /// Sets the [`Id`] of the [`ContextDrawer`].
    #[inline]
    pub fn id(mut self, id: iced_core::widget::Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Map the message type of the context drawer to another
    #[inline]
    pub fn map<Out: Clone + 'static>(
        self,
        on_message: fn(Message) -> Out,
    ) -> ContextDrawer<'a, Out> {
        ContextDrawer {
            id: self.id,
            content: self.content.map(on_message),
            drawer: self.drawer.map(on_message),
            on_close: self.on_close.map(on_message),
        }
    }

    /// Optionally assigns message to `on_close` event.
    #[inline]
    pub fn on_close_maybe(mut self, message: Option<Message>) -> Self {
        self.on_close = message;
        self
    }
}

impl<Message: Clone> Widget<Message, crate::ui::Theme, Renderer> for ContextDrawer<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.drawer)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content, &self.drawer]);
    }

    fn size(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<iced_overlay::Element<'b, Message, crate::ui::Theme, Renderer>> {
        let bounds = layout.bounds();

        let mut position = layout.position();
        position.x += translation.x;
        position.y += translation.y;

        Some(iced_overlay::Element::new(Box::new(Overlay {
            content: &mut self.drawer,
            tree: &mut tree.children[1],
            width: bounds.width,
            position,
        })))
    }
}

impl<'a, Message: 'a + Clone> From<ContextDrawer<'a, Message>> for Element<'a, Message> {
    fn from(widget: ContextDrawer<'a, Message>) -> Element<'a, Message> {
        Element::new(widget)
    }
}
