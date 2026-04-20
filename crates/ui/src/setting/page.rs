use std::rc::Rc;

use gpui::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    ParentElement as _, SharedString, StyleRefinement, Styled, Window, div, list,
    prelude::FluentBuilder as _, px,
};
use rust_i18n::t;

use crate::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    scroll::ScrollableElement,
    setting::{RenderOptions, SettingGroup, settings::SettingsState},
    v_flex,
};

/// Closure producing a fully custom right-hand content element for a
/// [`SettingPage`]. When set (via [`SettingPage::custom_content`]), the page
/// renders only its header (title + optional description + reset button is
/// suppressed) plus the element returned by this closure — the declarative
/// group/item/field pipeline is bypassed entirely. This is the escape hatch
/// for settings that don't fit the `SettingField` enum (e.g., a keybindings
/// editor with a `DataTable`).
pub type CustomContentFn = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// A setting page that can contain multiple setting groups.
#[derive(Clone)]
pub struct SettingPage {
    pub(super) icon: Option<Icon>,
    resettable: bool,
    pub(super) default_open: bool,
    pub(super) title: SharedString,
    pub(super) description: Option<SharedString>,
    pub(super) groups: Vec<SettingGroup>,
    pub(super) header_style: StyleRefinement,
    /// Optional custom render — if set, replaces the groups-list pipeline.
    pub(super) custom_content: Option<CustomContentFn>,
}

impl SettingPage {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            resettable: true,
            default_open: false,
            title: title.into(),
            description: None,
            groups: Vec::new(),
            header_style: StyleRefinement::default(),
            custom_content: None,
        }
    }

    /// Replace the right-hand content with a fully custom element produced by
    /// `render`. The page still appears in the sidebar under its title/icon,
    /// but the groups/items pipeline is bypassed when this page is selected.
    ///
    /// Use this for settings UIs that don't map to the `SettingField` enum —
    /// e.g., a keybindings table.
    pub fn custom_content<F>(mut self, render: F) -> Self
    where
        F: Fn(&mut Window, &mut App) -> AnyElement + 'static,
    {
        self.custom_content = Some(Rc::new(render));
        self
    }

    /// Returns true if this page uses a custom render closure instead of the
    /// default groups/items pipeline.
    pub(super) fn has_custom_content(&self) -> bool {
        self.custom_content.is_some()
    }

    /// Set the title of the setting page.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the icon of the setting page.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the description of the setting page, default is None.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the default open state of the setting page, default is false.
    pub fn default_open(mut self, default_open: bool) -> Self {
        self.default_open = default_open;
        self
    }

    /// Set whether the setting page is resettable, default is true.
    ///
    /// If true and the items in this page has changed, the reset button will appear.
    pub fn resettable(mut self, resettable: bool) -> Self {
        self.resettable = resettable;
        self
    }

    /// Add a setting group to the page.
    pub fn group(mut self, group: SettingGroup) -> Self {
        self.groups.push(group);
        self
    }

    /// Add multiple setting groups to the page.
    pub fn groups(mut self, groups: impl IntoIterator<Item = SettingGroup>) -> Self {
        self.groups.extend(groups);
        self
    }

    /// Set the style refinement for the header of the setting page.
    pub fn header_style(mut self, style: &StyleRefinement) -> Self {
        self.header_style = style.clone();
        self
    }

    fn is_resettable(&self, cx: &App) -> bool {
        self.resettable && self.groups.iter().any(|group| group.is_resettable(cx))
    }

    fn reset_all(&self, window: &mut Window, cx: &mut App) {
        for group in &self.groups {
            group.reset(window, cx);
        }
    }

    pub(super) fn render(
        &self,
        ix: usize,
        state: &Entity<SettingsState>,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        // Custom-content pages bypass the groups/items pipeline entirely. They
        // still render the header (title + optional description), but the body
        // is whatever the closure returns. Reset-all is suppressed since
        // custom pages own their own reset affordances.
        if let Some(custom) = self.custom_content.clone() {
            return v_flex()
                .id(ix)
                .size_full()
                .child(
                    v_flex()
                        .p_4()
                        .gap_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .refine_style(&self.header_style)
                        .child(h_flex().justify_between().child(self.title.clone()))
                        .when_some(self.description.clone(), |this, description| {
                            this.child(
                                Label::new(description)
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground),
                            )
                        }),
                )
                .child(div().flex_1().w_full().child(custom(window, cx)))
                .into_any_element();
        }

        let search_input = state.read(cx).search_input.clone();
        let query = search_input.read(cx).value();
        let groups = self
            .groups
            .iter()
            .filter(|group| group.is_match(&query, cx))
            .cloned()
            .collect::<Vec<_>>();
        let groups_count = groups.len();

        let list_state = window
            .use_keyed_state(
                SharedString::from(format!("list-state:{}", ix)),
                cx,
                |_, _| ListState::new(groups_count, ListAlignment::Top, px(100.)),
            )
            .read(cx)
            .clone();

        if list_state.item_count() != groups_count {
            list_state.reset(groups_count);
        }

        let deferred_scroll_group_ix = state.read(cx).deferred_scroll_group_ix;
        if let Some(ix) = deferred_scroll_group_ix {
            state.update(cx, |state, _| {
                state.deferred_scroll_group_ix = None;
            });
            list_state.scroll_to_reveal_item(ix);
        }

        v_flex()
            .id(ix)
            .size_full()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .refine_style(&self.header_style)
                    .child(h_flex().justify_between().child(self.title.clone()).when(
                        self.is_resettable(cx),
                        |this| {
                            this.child(
                                Button::new("reset")
                                    .icon(IconName::Undo2)
                                    .ghost()
                                    .small()
                                    .tooltip(t!("Settings.Reset All"))
                                    .on_click({
                                        let page = self.clone();
                                        move |_, window, cx| {
                                            page.reset_all(window, cx);
                                            // The reset button disappears after reset (no longer
                                            // resettable), so focus would be lost. Move focus to
                                            // the next available element.
                                            window.focus_next(cx);
                                        }
                                    }),
                            )
                        },
                    ))
                    .when_some(self.description.clone(), |this, description| {
                        this.child(
                            Label::new(description)
                                .text_sm()
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            )
            .child(
                div()
                    .px_4()
                    .relative()
                    .flex_1()
                    .w_full()
                    .child(
                        list(list_state.clone(), {
                            let query = query.clone();
                            let options = *options;
                            move |group_ix, window, cx| {
                                let group = groups[group_ix].clone();
                                group
                                    .py_4()
                                    .render(
                                        &query,
                                        &RenderOptions {
                                            page_ix: ix,
                                            group_ix,
                                            ..options
                                        },
                                        window,
                                        cx,
                                    )
                                    .into_any_element()
                            }
                        })
                        .size_full(),
                    )
                    .vertical_scrollbar(&list_state),
            )
            .into_any_element()
    }
}
