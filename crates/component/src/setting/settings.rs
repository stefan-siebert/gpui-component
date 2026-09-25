use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use crate::{
    IconName, Sizable, Size, StyledExt,
    actions::{Confirm, SelectDown, SelectFirst, SelectLast, SelectLeft, SelectRight, SelectUp},
    group_box::GroupBoxVariant,
    h_resizable,
    input::{Input, InputState},
    resizable_panel,
    setting::SettingPage,
    sidebar::{Sidebar, SidebarMenu, SidebarMenuItem},
};
use gpui::{
    App, AppContext as _, Axis, ElementId, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Pixels, RenderOnce, SharedString, StyleRefinement,
    Styled, Window, container_query, div, prelude::FluentBuilder as _, px, relative,
};
use rust_i18n::t;

const STACKED_LAYOUT_MAX_WIDTH: Pixels = px(480.);

/// Key context of the navigation list, active while an entry has focus.
const NAV_CONTEXT: &str = "SettingsNav";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some(NAV_CONTEXT)),
        KeyBinding::new("down", SelectDown, Some(NAV_CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(NAV_CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(NAV_CONTEXT)),
        KeyBinding::new("right", SelectRight, Some(NAV_CONTEXT)),
        KeyBinding::new("left", SelectLeft, Some(NAV_CONTEXT)),
        KeyBinding::new("enter", Confirm { secondary: false }, Some(NAV_CONTEXT)),
        KeyBinding::new("space", Confirm { secondary: false }, Some(NAV_CONTEXT)),
    ]);
}

type PageChangeFn = Rc<dyn Fn(usize, &mut App)>;

/// Identifies one entry of the navigation list.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct NavKey {
    page_ix: usize,
    /// Index among the page's titled groups; `None` for the page itself.
    group_ix: Option<usize>,
}

/// One visible entry of the navigation list, in display order.
struct NavEntry {
    key: NavKey,
    /// The page's title, which keys its open state.
    page_title: SharedString,
    has_submenu: bool,
    is_open: bool,
    focus_handle: FocusHandle,
}

/// The navigation list as rendered, for the keyboard handlers.
///
/// Exactly one entry is a tab stop (a roving tabindex): the selected one. Tab
/// therefore leaves the list in one step, and the arrow keys move within it.
struct Nav {
    entries: Vec<NavEntry>,
    tab_stop: usize,
}

impl Nav {
    fn build(
        pages: &[SettingPage],
        filter: &SettingsFilter,
        state: &mut SettingsState,
        cx: &mut App,
    ) -> Self {
        let mut entries = Vec::new();
        for page_ix in filter.visible_pages() {
            let page = &pages[page_ix];
            let groups = &filter.groups[page_ix];
            let has_submenu = groups.len() > 1;
            let is_open = has_submenu && state.is_page_open(page);
            entries.push(NavEntry {
                key: NavKey {
                    page_ix,
                    group_ix: None,
                },
                page_title: page.title.clone(),
                has_submenu,
                is_open,
                focus_handle: state.nav_focus_handle(
                    NavKey {
                        page_ix,
                        group_ix: None,
                    },
                    cx,
                ),
            });
            if is_open {
                for &group_ix in groups
                    .iter()
                    .filter(|&&group_ix| page.groups[group_ix].title.is_some())
                {
                    let key = NavKey {
                        page_ix,
                        group_ix: Some(group_ix),
                    };
                    entries.push(NavEntry {
                        key,
                        page_title: page.title.clone(),
                        has_submenu: false,
                        is_open: false,
                        focus_handle: state.nav_focus_handle(key, cx),
                    });
                }
            }
        }

        let selected = state.selected_index;
        let position = |group_ix| {
            entries.iter().position(|entry| {
                entry.key
                    == NavKey {
                        page_ix: selected.page_ix,
                        group_ix,
                    }
            })
        };
        // A selected group whose page is collapsed hands the stop to the page.
        let tab_stop = position(selected.group_ix)
            .or_else(|| position(None))
            .unwrap_or(0);

        Self { entries, tab_stop }
    }

    fn handle(&self, key: NavKey) -> Option<FocusHandle> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.focus_handle.clone())
    }

    fn is_tab_stop(&self, key: NavKey) -> bool {
        self.entries
            .get(self.tab_stop)
            .is_some_and(|entry| entry.key == key)
    }

    /// The entry with keyboard focus, or the tab stop if focus is elsewhere
    /// in the list (on a submenu caret the mouse left focused).
    fn focused(&self, window: &Window) -> usize {
        self.entries
            .iter()
            .position(|entry| entry.focus_handle.is_focused(window))
            .unwrap_or(self.tab_stop)
    }
}

/// Select a navigation entry, as a click on it does.
fn select_entry(
    state: &Entity<SettingsState>,
    key: NavKey,
    on_page_change: Option<&PageChangeFn>,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        state.selected_index = SelectIndex {
            page_ix: key.page_ix,
            group_ix: key.group_ix,
        };
        state.deferred_scroll_group_ix = key.group_ix;
        cx.notify();
    });
    if let Some(on_page_change) = on_page_change {
        on_page_change(key.page_ix, cx);
    }
}

/// The settings structure containing multiple pages for app settings.
///
/// The hierarchy of settings is as follows:
///
/// ```ignore
/// Settings
///   SettingPage     <- The single active page displayed
///     SettingGroup
///       SettingItem
///         Label
///         SettingField (e.g., Switch, Dropdown, Input)
/// ```
#[derive(IntoElement)]
pub struct Settings {
    id: ElementId,
    pages: Vec<SettingPage>,
    group_variant: GroupBoxVariant,
    size: Size,
    sidebar_width: Pixels,
    sidebar_size_range: Range<Pixels>,
    sidebar_style: StyleRefinement,
    content_style: StyleRefinement,
    default_selected_index: SelectIndex,
    header_style: StyleRefinement,
    /// If set, override the selected page on every render.
    force_page: Option<usize>,
    /// Callback when the selected page changes (via sidebar click or keyboard).
    on_page_change: Option<PageChangeFn>,
    /// Whether to auto-focus the search input on first render.
    auto_focus_search: bool,
}

impl Settings {
    /// Create a new settings with the given ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            pages: vec![],
            group_variant: GroupBoxVariant::default(),
            size: Size::default(),
            sidebar_width: px(250.0),
            sidebar_size_range: px(160.0)..px(360.0),
            sidebar_style: StyleRefinement::default(),
            content_style: StyleRefinement::default(),
            default_selected_index: SelectIndex::default(),
            header_style: StyleRefinement::default(),
            force_page: None,
            on_page_change: None,
            auto_focus_search: false,
        }
    }

    /// Set the initial width of the sidebar, default is `190px`.
    pub fn sidebar_width(mut self, width: impl Into<Pixels>) -> Self {
        self.sidebar_width = width.into();
        self
    }

    /// Set the min/max size range the sidebar can be resized to by the user,
    /// default is `160px..360px`. The upper bound must stay finite, or the user
    /// can drag the sidebar over the entire content panel.
    pub fn sidebar_size_range(mut self, range: impl Into<Range<Pixels>>) -> Self {
        self.sidebar_size_range = range.into();
        self
    }

    /// Add a page to the settings.
    pub fn page(mut self, page: SettingPage) -> Self {
        self.pages.push(page);
        self
    }

    /// Add pages to the settings.
    pub fn pages(mut self, pages: impl IntoIterator<Item = SettingPage>) -> Self {
        self.pages.extend(pages);
        self
    }

    /// Set the default variant for all setting groups.
    ///
    /// All setting groups will use this variant unless overridden individually.
    pub fn with_group_variant(mut self, variant: GroupBoxVariant) -> Self {
        self.group_variant = variant;
        self
    }

    /// Set the style refinement for the sidebar.
    pub fn sidebar_style(mut self, style: &StyleRefinement) -> Self {
        self.sidebar_style = style.clone();
        self
    }

    /// Set the style refinement for the panel holding the active page.
    ///
    /// Pages paint no background of their own, so they show whatever the
    /// window paints behind them. Set a background here when the window itself
    /// is translucent and the content must stay readable — e.g. a macOS
    /// translucent window whose sidebar is a material but whose content pane
    /// should be opaque.
    pub fn content_style(mut self, style: &StyleRefinement) -> Self {
        self.content_style = style.clone();
        self
    }

    /// Set the default index of the page to be selected.
    pub fn default_selected_index(mut self, index: SelectIndex) -> Self {
        self.default_selected_index = index;
        self
    }

    /// Set the style refinement for the header.
    pub fn header_style(mut self, style: &StyleRefinement) -> Self {
        self.header_style = style.clone();
        self
    }

    /// Force the selected page index on every render.
    ///
    /// Use this together with [`on_page_change`](Self::on_page_change) to drive
    /// page selection from an external View.
    pub fn force_page(mut self, page_ix: usize) -> Self {
        self.force_page = Some(page_ix);
        self
    }

    /// Set a callback invoked when the selected page changes (sidebar click or
    /// keyboard navigation).
    pub fn on_page_change(mut self, f: impl Fn(usize, &mut App) + 'static) -> Self {
        self.on_page_change = Some(Rc::new(f));
        self
    }

    /// Auto-focus the search input when the settings are first rendered.
    pub fn auto_focus_search(mut self) -> Self {
        self.auto_focus_search = true;
        self
    }

    fn render_active_page(
        &self,
        state: &Entity<SettingsState>,
        filter: &SettingsFilter,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let page_ix = state.read(cx).selected_index.page_ix;
        if let Some(page) = self.pages.get(page_ix)
            && filter.is_visible(page_ix)
        {
            return page
                .render(page_ix, &filter.groups[page_ix], state, options, window, cx)
                .into_any_element();
        }

        div().into_any_element()
    }

    fn render_sidebar(
        &self,
        state: &Entity<SettingsState>,
        filter: &SettingsFilter,
        nav: &Rc<Nav>,
        _: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let selected_index = state.read(cx).selected_index;
        let search_input = state.read(cx).search_input.clone();
        let on_page_change = self.on_page_change.clone();
        let entry_focus = |key: NavKey| {
            nav.handle(key).map(|handle| {
                let is_tab_stop = nav.is_tab_stop(key);
                handle.tab_index(0).tab_stop(is_tab_stop)
            })
        };

        let sidebar = Sidebar::new("settings-sidebar")
            .w(relative(1.))
            .border_0()
            .refine_style(&self.sidebar_style)
            .collapsible(false)
            .collapsed(false)
            .header(
                div()
                    .w_full()
                    .refine_style(&self.header_style)
                    .child(Input::new(&search_input).prefix(IconName::Search)),
            )
            .child(SidebarMenu::new().key_context(NAV_CONTEXT).children(
                filter.visible_pages().map(|page_ix| {
                    let page = &self.pages[page_ix];
                    let groups = &filter.groups[page_ix];
                    let page_key = NavKey {
                        page_ix,
                        group_ix: None,
                    };
                    let is_page_active = selected_index.page_ix == page_ix
                        && (selected_index.group_ix.is_none() || groups.len() == 1);
                    SidebarMenuItem::new(page.title.clone())
                        .click_to_open(true)
                        .when_some(page.icon.clone(), |this, icon| this.icon(icon))
                        .when_some(entry_focus(page_key), |this, handle| {
                            this.track_focus(&handle)
                        })
                        .when(groups.len() > 1, |this| {
                            let page_title = page.title.clone();
                            let state = state.clone();
                            this.open(state.read(cx).is_page_open(page)).on_open_change(
                                move |open, _, cx| {
                                    state.update(cx, |state, cx| {
                                        state.open_pages.insert(page_title.clone(), open);
                                        cx.notify();
                                    })
                                },
                            )
                        })
                        .active(is_page_active)
                        .on_click({
                            let state = state.clone();
                            let on_page_change = on_page_change.clone();
                            move |_, _, cx| {
                                select_entry(&state, page_key, on_page_change.as_ref(), cx)
                            }
                        })
                        .when(groups.len() > 1, |this| {
                            this.children(
                                groups
                                    .iter()
                                    .copied()
                                    .filter(|&ix| page.groups[ix].title.is_some())
                                    .map(|group_ix| {
                                        let group = &page.groups[group_ix];
                                        let group_key = NavKey {
                                            page_ix,
                                            group_ix: Some(group_ix),
                                        };
                                        let is_active = selected_index.page_ix == page_ix
                                            && selected_index.group_ix == Some(group_ix);
                                        let title = group.title.clone().unwrap_or_default();

                                        SidebarMenuItem::new(title)
                                            .when_some(entry_focus(group_key), |this, handle| {
                                                this.track_focus(&handle)
                                            })
                                            .active(is_active)
                                            .on_click({
                                                let state = state.clone();
                                                let on_page_change = on_page_change.clone();
                                                move |_, _, cx| {
                                                    select_entry(
                                                        &state,
                                                        group_key,
                                                        on_page_change.as_ref(),
                                                        cx,
                                                    )
                                                }
                                            })
                                    }),
                            )
                        })
                }),
            ));

        let on_page_change = self.on_page_change.clone();
        let handlers = Rc::new(NavHandlers {
            state: state.clone(),
            nav: nav.clone(),
            on_page_change,
        });
        div()
            .size_full()
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectUp, window, cx| handlers.step(-1, window, cx)
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectDown, window, cx| handlers.step(1, window, cx)
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectFirst, window, cx| handlers.move_to(0, window, cx)
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectLast, window, cx| {
                    let last = handlers.nav.entries.len().saturating_sub(1);
                    handlers.move_to(last, window, cx)
                }
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectRight, window, cx| handlers.expand_or_enter(window, cx)
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &SelectLeft, window, cx| handlers.collapse_or_parent(window, cx)
            })
            .on_action({
                let handlers = handlers.clone();
                move |_: &Confirm, window, cx| handlers.activate(window, cx)
            })
            .child(sidebar)
    }
}

/// The navigation list's keyboard handlers, over the list as last rendered.
struct NavHandlers {
    state: Entity<SettingsState>,
    nav: Rc<Nav>,
    on_page_change: Option<PageChangeFn>,
}

impl NavHandlers {
    /// ↑ / ↓: the neighbouring entry, which switches the page at once.
    fn step(&self, delta: isize, window: &mut Window, cx: &mut App) {
        cx.stop_propagation();
        let target = self.nav.focused(window).saturating_add_signed(delta);
        if target < self.nav.entries.len() {
            self.move_to(target, window, cx);
        }
    }

    fn move_to(&self, ix: usize, window: &mut Window, cx: &mut App) {
        cx.stop_propagation();
        let Some(entry) = self.nav.entries.get(ix) else {
            return;
        };
        select_entry(&self.state, entry.key, self.on_page_change.as_ref(), cx);
        entry.focus_handle.focus(window, cx);
    }

    /// →: open a closed submenu, otherwise go into the content pane.
    fn expand_or_enter(&self, window: &mut Window, cx: &mut App) {
        cx.stop_propagation();
        let Some(entry) = self.nav.entries.get(self.nav.focused(window)) else {
            return;
        };
        if entry.has_submenu && !entry.is_open {
            self.set_open(entry, true, cx);
        } else {
            self.enter_content(entry, cx);
        }
    }

    /// ←: close an open submenu, otherwise go to the parent entry.
    fn collapse_or_parent(&self, window: &mut Window, cx: &mut App) {
        cx.stop_propagation();
        let Some(entry) = self.nav.entries.get(self.nav.focused(window)) else {
            return;
        };
        if entry.key.group_ix.is_some() {
            let parent = NavKey {
                group_ix: None,
                ..entry.key
            };
            if let Some(parent_ix) = self.nav.entries.iter().position(|e| e.key == parent) {
                self.move_to(parent_ix, window, cx);
            }
        } else if entry.is_open {
            self.set_open(entry, false, cx);
        }
    }

    /// Enter / Space: select the entry and go into the content pane.
    ///
    /// The entry's own keyboard click never fires on top of this: gpui maps
    /// Enter / Space to a click on the key-*up*, and only while focus has not
    /// moved since the key-down (`Window::focus_generation`). This action
    /// runs on the key-down and moves focus, so the click is dropped — which
    /// is what keeps the page from being selected twice. Do not "simplify"
    /// this into an `on_click`.
    fn activate(&self, window: &mut Window, cx: &mut App) {
        cx.stop_propagation();
        let Some(entry) = self.nav.entries.get(self.nav.focused(window)) else {
            return;
        };
        self.enter_content(entry, cx);
    }

    fn set_open(&self, entry: &NavEntry, open: bool, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            state.open_pages.insert(entry.page_title.clone(), open);
            cx.notify();
        });
    }

    fn enter_content(&self, entry: &NavEntry, cx: &mut App) {
        let selected = self.state.read(cx).selected_index;
        if selected.page_ix != entry.key.page_ix || selected.group_ix != entry.key.group_ix {
            select_entry(&self.state, entry.key, self.on_page_change.as_ref(), cx);
        }
        self.state.update(cx, |state, cx| {
            state.enter_content_pending = true;
            cx.notify();
        });
    }
}

impl Sizable for Settings {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

/// Visible groups in each original page. Filtering never renumbers source data.
struct SettingsFilter {
    groups: Vec<Vec<usize>>,
    /// Fork: pages with custom content own their search, so the outer query
    /// never hides them (they have no groups to match against).
    custom: Vec<bool>,
}

impl SettingsFilter {
    fn new(pages: &[SettingPage], query: &str, cx: &App) -> Self {
        Self {
            groups: pages
                .iter()
                .map(|page| {
                    page.groups
                        .iter()
                        .enumerate()
                        .filter_map(|(ix, group)| group.is_match(query, cx).then_some(ix))
                        .collect()
                })
                .collect(),
            custom: pages.iter().map(SettingPage::has_custom_content).collect(),
        }
    }

    fn is_visible(&self, page_ix: usize) -> bool {
        self.custom.get(page_ix).copied().unwrap_or(false)
            || self
                .groups
                .get(page_ix)
                .is_some_and(|groups| !groups.is_empty())
    }

    fn visible_pages(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.groups.len()).filter(|&ix| self.is_visible(ix))
    }

    fn selected_index(&self, selected: SelectIndex) -> SelectIndex {
        let page_ix = self
            .visible_pages()
            .find(|&ix| ix == selected.page_ix)
            .or_else(|| self.visible_pages().next());
        let Some(page_ix) = page_ix else {
            // Keep the selection while there are no results so clearing the query
            // can restore it. The empty filter prevents rendering a stale page.
            return selected;
        };

        SelectIndex {
            page_ix,
            group_ix: selected
                .group_ix
                .filter(|ix| page_ix == selected.page_ix && self.groups[page_ix].contains(ix)),
        }
    }
}

pub struct SettingsState {
    pub selected_index: SelectIndex,
    /// If set, defer scrolling to this group index after rendering.
    pub deferred_scroll_group_ix: Option<usize>,
    pub search_input: Entity<InputState>,
    /// Track whether auto-focus has been applied.
    auto_focused: bool,
    /// Submenus opened or closed by the user, by page title. A page not in
    /// here follows its `default_open`.
    open_pages: HashMap<SharedString, bool>,
    /// Focus handles of the navigation entries.
    nav_focus: HashMap<NavKey, FocusHandle>,
    /// Focusable but not a tab stop: focusing it and then moving to the next
    /// tab stop lands on the first control of the active page.
    content_focus: FocusHandle,
    /// Move focus into the content pane once the page it shows is drawn.
    enter_content_pending: bool,
}

impl SettingsState {
    fn is_page_open(&self, page: &SettingPage) -> bool {
        self.open_pages
            .get(&page.title)
            .copied()
            .unwrap_or(page.default_open)
    }

    fn nav_focus_handle(&mut self, key: NavKey, cx: &mut App) -> FocusHandle {
        self.nav_focus
            .entry(key)
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }
}

/// Options for rendering setting item.
///
/// The fields are private and reached through the methods below, so that a new
/// one can be added without breaking the item renderers. The setters take
/// `self` by value, so a nested renderer narrows a copy of its parent options:
///
/// ```ignore
/// item.render_item(&options.with_item_ix(item_ix), window, cx)
/// ```
#[derive(Clone, Copy)]
pub struct RenderOptions {
    page_ix: usize,
    group_ix: usize,
    item_ix: usize,
    size: Size,
    group_variant: GroupBoxVariant,
    layout: Axis,
    disabled: bool,
}

impl RenderOptions {
    pub fn new() -> Self {
        Self {
            page_ix: 0,
            group_ix: 0,
            item_ix: 0,
            size: Size::default(),
            group_variant: GroupBoxVariant::default(),
            layout: Axis::Horizontal,
            disabled: false,
        }
    }

    pub fn with_page_ix(mut self, page_ix: usize) -> Self {
        self.page_ix = page_ix;
        self
    }

    pub fn with_group_ix(mut self, group_ix: usize) -> Self {
        self.group_ix = group_ix;
        self
    }

    pub fn with_item_ix(mut self, item_ix: usize) -> Self {
        self.item_ix = item_ix;
        self
    }

    pub fn with_size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn with_group_variant(mut self, group_variant: GroupBoxVariant) -> Self {
        self.group_variant = group_variant;
        self
    }

    pub fn with_layout(mut self, layout: Axis) -> Self {
        self.layout = layout;
        self
    }

    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn page_ix(&self) -> usize {
        self.page_ix
    }

    pub fn group_ix(&self) -> usize {
        self.group_ix
    }

    pub fn item_ix(&self) -> usize {
        self.item_ix
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn group_variant(&self) -> GroupBoxVariant {
        self.group_variant
    }

    pub fn layout(&self) -> Axis {
        self.layout
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Default)]
pub struct SelectIndex {
    pub page_ix: usize,
    pub group_ix: Option<usize>,
}

impl RenderOnce for Settings {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |window, cx| {
            let search_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(t!("Settings.search_placeholder"))
                    .default_value("")
            });

            SettingsState {
                search_input,
                selected_index: self.default_selected_index,
                deferred_scroll_group_ix: None,
                auto_focused: false,
                open_pages: HashMap::new(),
                nav_focus: HashMap::new(),
                content_focus: cx.focus_handle(),
                enter_content_pending: false,
            }
        });

        // Auto-focus search input on first render
        if self.auto_focus_search && !state.read(cx).auto_focused {
            let search = state.read(cx).search_input.clone();
            search.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            state.update(cx, |s, _cx| {
                s.auto_focused = true;
            });
        }

        // Force page selection if requested
        if let Some(page_ix) = self.force_page {
            let current = state.read(cx).selected_index.page_ix;
            if current != page_ix {
                state.update(cx, |s, cx| {
                    s.selected_index = SelectIndex {
                        page_ix,
                        group_ix: None,
                    };
                    cx.notify();
                });
            }
        }

        let query = state.read(cx).search_input.read(cx).value();
        let filter = SettingsFilter::new(&self.pages, &query, cx);
        let previous = state.read(cx).selected_index;
        let selected = filter.selected_index(previous);
        if selected.page_ix != previous.page_ix || selected.group_ix != previous.group_ix {
            state.update(cx, |state, _| {
                state.selected_index = selected;
                state.deferred_scroll_group_ix = None;
            });
        }
        let options = RenderOptions::new()
            .with_size(self.size)
            .with_group_variant(self.group_variant);
        let sidebar_size_range = self.sidebar_size_range.clone();
        let nav =
            Rc::new(state.update(cx, |state, cx| Nav::build(&self.pages, &filter, state, cx)));
        let content_focus = state.read(cx).content_focus.clone();

        // Tab stops are read from the last drawn frame, and the page that
        // focus should enter is being drawn only now — so go in on the next
        // frame. A page without any focusable control gives focus back to the
        // navigation instead of stranding it on the invisible container.
        if state.read(cx).enter_content_pending {
            state.update(cx, |state, _| state.enter_content_pending = false);
            let content_focus = content_focus.clone();
            let entry_focus = nav
                .entries
                .get(nav.tab_stop)
                .map(|entry| entry.focus_handle.clone());
            window.on_next_frame(move |window, cx| {
                content_focus.focus(window, cx);
                window.focus_next(cx);
                let landed_inside =
                    content_focus.contains_focused(window, cx) && !content_focus.is_focused(window);
                if !landed_inside && let Some(entry_focus) = entry_focus {
                    entry_focus.focus(window, cx);
                }
            });
        }

        let sidebar = self
            .render_sidebar(&state, &filter, &nav, window, cx)
            .into_any_element();

        h_resizable(self.id.clone())
            .child(
                resizable_panel()
                    .size(self.sidebar_width)
                    .size_range(sidebar_size_range)
                    .child(sidebar),
            )
            .child(
                resizable_panel()
                    // `content_style` is a fork addition (7c5fc117): Elane's
                    // translucent settings window styles the content pane
                    // through it, so the refinement must reach the panel.
                    .refine_style(&self.content_style)
                    .child(
                        div()
                            .id("settings-content")
                            .track_focus(&content_focus.tab_index(0).tab_stop(false))
                            .size_full()
                            .child(container_query(move |size, window, cx| {
                                let options = options.with_layout(
                                    if size.width <= STACKED_LAYOUT_MAX_WIDTH {
                                        Axis::Vertical
                                    } else {
                                        Axis::Horizontal
                                    },
                                );
                                self.render_active_page(&state, &filter, &options, window, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod nav_tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use gpui::{
        Context, KeyDownEvent, KeyUpEvent, Keystroke, Render, TestAppContext, VisualTestContext,
    };

    use super::*;
    use crate::setting::{SettingField, SettingGroup, SettingItem, SettingPage};

    struct SettingsHarness {
        page_changes: Rc<RefCell<Vec<usize>>>,
        toggled: Rc<RefCell<Vec<&'static str>>>,
    }

    impl SettingsHarness {
        fn switch(&self, name: &'static str) -> SettingItem {
            let toggled = self.toggled.clone();
            SettingItem::new(
                name,
                SettingField::switch(|_| false, move |_, _| toggled.borrow_mut().push(name)),
            )
        }
    }

    impl Render for SettingsHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let page_changes = self.page_changes.clone();
            div().size(px(800.)).child(
                Settings::new("settings")
                    .page(
                        SettingPage::new("General")
                            .group(
                                SettingGroup::new()
                                    .title("Files")
                                    .item(self.switch("files")),
                            )
                            .group(
                                SettingGroup::new()
                                    .title("Index")
                                    .item(self.switch("index")),
                            ),
                    )
                    .page(
                        SettingPage::new("Panels")
                            .group(SettingGroup::new().item(self.switch("panels"))),
                    )
                    .page(
                        SettingPage::new("Network")
                            .group(SettingGroup::new().item(self.switch("network"))),
                    )
                    .on_page_change(move |page_ix, _| page_changes.borrow_mut().push(page_ix)),
            )
        }
    }

    fn harness(
        cx: &mut TestAppContext,
    ) -> (
        &mut VisualTestContext,
        Rc<RefCell<Vec<usize>>>,
        Rc<RefCell<Vec<&'static str>>>,
    ) {
        cx.update(crate::init);
        let page_changes = Rc::new(RefCell::new(Vec::new()));
        let toggled = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let page_changes = page_changes.clone();
            let toggled = toggled.clone();
            move |_, _| SettingsHarness {
                page_changes,
                toggled,
            }
        });
        settle(cx);
        (cx, page_changes, toggled)
    }

    /// Draw, then deliver the next frame, which is when focus enters the
    /// content pane.
    fn settle(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
    }

    fn tab(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.focus_next(cx));
        settle(cx);
    }

    fn key(cx: &mut VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        settle(cx);
    }

    #[gpui::test]
    fn one_tab_leads_from_the_navigation_into_the_content(cx: &mut TestAppContext) {
        let (cx, page_changes, toggled) = harness(cx);
        // Search input, then the selected entry — the only tab stop of the list.
        tab(cx);
        tab(cx);
        tab(cx);

        key(cx, "space");

        assert_eq!(*toggled.borrow(), ["files"]);
        assert!(page_changes.borrow().is_empty());
    }

    #[gpui::test]
    fn arrows_move_through_the_navigation_and_switch_the_page(cx: &mut TestAppContext) {
        let (cx, page_changes, _) = harness(cx);
        tab(cx);
        tab(cx);

        key(cx, "down");
        key(cx, "down");
        key(cx, "down");
        assert_eq!(
            *page_changes.borrow(),
            [1, 2],
            "no step past the last entry"
        );

        key(cx, "home");
        key(cx, "end");
        assert_eq!(*page_changes.borrow(), [1, 2, 0, 2]);
    }

    #[gpui::test]
    fn right_opens_a_submenu_and_left_returns_to_its_page(cx: &mut TestAppContext) {
        let (cx, page_changes, _) = harness(cx);
        tab(cx);
        tab(cx);

        key(cx, "right");
        assert!(page_changes.borrow().is_empty(), "opening does not select");
        key(cx, "down");
        assert_eq!(*page_changes.borrow(), [0], "the first group of General");
        key(cx, "left");
        assert_eq!(*page_changes.borrow(), [0, 0], "back on General");
        key(cx, "left");
        key(cx, "down");
        assert_eq!(*page_changes.borrow(), [0, 0, 1], "closed: Panels is next");
    }

    #[gpui::test]
    fn enter_selects_the_page_once_and_moves_into_its_content(cx: &mut TestAppContext) {
        let (cx, page_changes, toggled) = harness(cx);
        tab(cx);
        tab(cx);
        key(cx, "down");

        key(cx, "enter");
        assert_eq!(
            *page_changes.borrow(),
            [1],
            "the entry's keyboard click must not select the page a second time"
        );
        assert!(
            toggled.borrow().is_empty(),
            "the key-up must not reach the control focus moved to"
        );

        key(cx, "space");
        assert_eq!(*toggled.borrow(), ["panels"]);
    }

    #[gpui::test]
    fn right_on_an_entry_without_submenu_moves_into_the_content(cx: &mut TestAppContext) {
        let (cx, _, toggled) = harness(cx);
        tab(cx);
        tab(cx);
        key(cx, "end");

        key(cx, "right");
        key(cx, "space");

        assert_eq!(*toggled.borrow(), ["network"]);
    }
}
