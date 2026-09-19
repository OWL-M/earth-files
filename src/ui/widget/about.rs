// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! This app's About section.
//!
//! Not a vendored copy: `libcosmic::widget::about` cannot be vendored. Its
//! `fl!` calls expand to `$crate::localize::…`, and libcosmic's `localize`
//! module is private, so a copy fails to compile with `error[E0603]`. The
//! layout below reproduces `libcosmic/src/widget/about.rs` (rev `d9431dc`)
//! element for element, built from this app's vendored widgets and localised
//! through this app's own `fl!` (`i18n/en/cosmic_files.ftl`).
//!
//! Because libcosmic's strings are unreachable, the section headings come from
//! this app's catalog and may differ in wording from libcosmic's.
//!
//! Trimmed against upstream: the `artists`, `designers`, `documenters` and
//! `translators` sections are gone. This app never populates them, and
//! `fl!` validates its `message_id` at compile time, so carrying them would
//! mean four never-rendered strings shipped to Weblate for translation.

use crate::fl;
use crate::ui::iced::{Alignment, ContentFit, Length};
use crate::ui::widget::{self, list};
use crate::ui::{Apply, Element};
use std::rc::Rc;
use crate::ui::convert::{PushMaybe, ToColor, ToPixels};
use crate::ui::convert::{ToPadding};

#[derive(Debug, Default, Clone, derive_setters::Setters)]
#[setters(into, strip_option)]
/// Information about the application.
pub struct About {
    /// The application's name.
    name: Option<String>,
    /// The application's icon.
    icon: Option<widget::icon::Handle>,
    /// The application's version.
    version: Option<String>,
    /// Name of the application's author.
    author: Option<String>,
    /// Comments about the application.
    comments: Option<String>,
    /// The application's copyright.
    copyright: Option<String>,
    /// The license name.
    license: Option<String>,
    /// The license url.
    license_url: Option<String>,
    /// Developers who contributed to the application.
    #[setters(skip)]
    developers: Vec<(String, String)>,
    /// Links associated with the application.
    #[setters(skip)]
    links: Vec<(String, String)>,
}

fn add_contributors(contributors: Vec<(&str, &str)>) -> Vec<(String, String)> {
    contributors
        .into_iter()
        .map(|(name, email)| (name.into(), format!("mailto:{email}")))
        .collect()
}

impl<'a> About {
    /// Developers who contributed to the application.
    #[must_use]
    pub fn developers(mut self, contributors: impl Into<Vec<(&'a str, &'a str)>>) -> Self {
        self.developers = add_contributors(contributors.into());
        self
    }

    /// Links associated with the application.
    #[must_use]
    pub fn links<K: Into<String>, V: Into<String>>(
        mut self,
        links: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.links = links
            .into_iter()
            .map(|(name, url)| (name.into(), url.into()))
            .collect();
        self
    }
}

/// Constructs the widget for the about section.
pub fn about<'a, Message: Clone + 'static>(
    about: &'a About,
    on_url_press: impl Fn(&'a str) -> Message + 'a,
) -> Element<'a, Message> {
    let crate::ui::theme::Spacing {
        space_xxs, space_m, ..
    } = crate::ui::theme::spacing();

    let svg_accent = Rc::new(|theme: &crate::ui::Theme| iced::widget::svg::Style {
        color: Some(theme.cosmic().accent_text_color().to_color()),
    });

    let section_button = |name: &'a str, url: &'a str| -> list::ListButton<'a, Message> {
        widget::Row::with_capacity(2)
            .push(widget::text::body(name).width(Length::Fill))
            .push_maybe(
                (!url.is_empty()).then_some(
                    widget::icon::from_name("link-symbolic")
                        .icon()
                        .class(crate::ui::theme::Svg::Custom(svg_accent.clone())),
                ),
            )
            .align_y(Alignment::Center)
            .apply(list::button)
            .on_press(on_url_press(url))
    };

    let section = |list: &'a Vec<(String, String)>, title: String| {
        (!list.is_empty()).then_some({
            let items = list.iter().map(|(name, url)| section_button(name, url));
            widget::settings::section().title(title).extend(items)
        })
    };

    let header_children: Vec<Element<'a, Message>> = [
        about.icon.as_ref().map(|i| {
            i.clone()
                .icon()
                .size(256)
                .width(Length::Fixed(128.))
                .height(Length::Fixed(128.))
                .content_fit(ContentFit::Contain)
                .into()
        }),
        about.name.as_ref().map(|n| widget::text::title3(n).into()),
        about.author.as_ref().map(|a| widget::text::body(a).into()),
        about.version.as_ref().map(|v| {
            widget::button::standard(v)
                .apply(widget::container)
                .padding(([space_xxs, 0, 0, 0]).to_padding())
                .into()
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    let header = (!header_children.is_empty())
        .then_some(widget::Column::with_children(header_children).align_x(Alignment::Center));

    let links_section = section(&about.links, fl!("links"));
    let developers_section = section(&about.developers, fl!("developers"));
    let license_section = about.license.as_ref().map(|license| {
        let url = about.license_url.as_deref().unwrap_or_default();
        widget::settings::section()
            .title(fl!("license"))
            .add(section_button(license, url))
    });
    let copyright = about.copyright.as_ref().map(widget::text::body);
    let comments = about.comments.as_ref().map(widget::text::body);

    widget::Column::with_capacity(6)
        .push_maybe(header)
        .push_maybe(links_section)
        .push_maybe(developers_section)
        .push_maybe(license_section)
        .push_maybe(comments)
        .push_maybe(copyright)
        .spacing(space_m.to_pixels())
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .into()
}
