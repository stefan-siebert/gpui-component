use std::{cell::RefCell, rc::Rc};

use gpui::{
    Anchor, AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, RenderOnce, Stateful,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Task, Window, div,
    prelude::FluentBuilder as _,
};
use instant::Duration;

use crate::{Popup, StyledExt as _};

type ContentBuilder = Box<
    dyn FnOnce(
        &mut HoverCardState,
        &mut Window,
        &mut Context<HoverCardState>,
    ) -> Stateful<gpui::Div>,
>;
type OpenChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// An unstyled popup with delayed hover behavior on desktop.
/// On iOS and Android, tapping toggles it and tapping outside dismisses it.
#[derive(IntoElement)]
pub struct HoverCard {
    id: ElementId,
    style: StyleRefinement,
    anchor: Anchor,
    tap_to_open: bool,
    trigger: Option<AnyElement>,
    content: Option<ContentBuilder>,
    open_delay: Duration,
    close_delay: Duration,
    content_offset: Point<Pixels>,
    interactive: bool,
    click_through: bool,
    on_open_change: Option<OpenChangeHandler>,
}

impl HoverCard {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            anchor: Anchor::TopCenter,
            tap_to_open: crate::is_mobile(),
            trigger: None,
            content: None,
            open_delay: Duration::from_secs_f64(0.6),
            close_delay: Duration::from_secs_f64(0.3),
            content_offset: Point::default(),
            interactive: true,
            click_through: false,
            on_open_change: None,
        }
    }

    /// A card that lies over things the user still wants to reach, default
    /// `false`.
    ///
    /// Such a card stays open while the mouse is on it, like any interactive
    /// card, so its controls can be reached — but it keeps only the presses
    /// its controls claim. A control claims a press by stopping it from
    /// propagating; it must not occlude the mouse, or the card under it no
    /// longer counts as hovered and closes. Any other press on the card
    /// closes it and is handed on to what the card covered: a picture over a
    /// list must not put the rows under it out of reach.
    ///
    /// The card still blocks the mouse while it is open, so nothing under it
    /// shows a hover colour or a tooltip through it.
    pub fn click_through(mut self, click_through: bool) -> Self {
        self.click_through = click_through;
        self
    }

    /// Whether the card itself can be hovered and clicked, default `true`.
    ///
    /// An interactive card stays open while the mouse is on it and takes the
    /// clicks that land there, which is what a card with links or buttons
    /// needs. A card that only shows something sets this to `false`: it is
    /// open exactly as long as the trigger is hovered, and the mouse goes
    /// through it to what it covers.
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    pub fn anchor(mut self, anchor: impl Into<Anchor>) -> Self {
        self.anchor = anchor.into();
        self
    }

    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.trigger = Some(trigger.into_any_element());
        self
    }

    /// Shift the card by this offset relative to its anchor point.
    /// See [`Popup::content_offset`].
    pub fn content_offset(mut self, offset: Point<Pixels>) -> Self {
        self.content_offset = offset;
        self
    }

    pub fn content<F>(mut self, content: F) -> Self
    where
        F: FnOnce(
                &mut HoverCardState,
                &mut Window,
                &mut Context<HoverCardState>,
            ) -> Stateful<gpui::Div>
            + 'static,
    {
        self.content = Some(Box::new(content));
        self
    }

    pub fn open_delay(mut self, duration: Duration) -> Self {
        self.open_delay = duration;
        self
    }

    pub fn close_delay(mut self, duration: Duration) -> Self {
        self.close_delay = duration;
        self
    }

    pub fn on_open_change(
        mut self,
        callback: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(callback));
        self
    }
}

/// Styles the trigger container: the element that takes part in the parent
/// layout and whose bounds the card is anchored to.
impl Styled for HoverCard {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// State exposed to a [`HoverCard::content`] builder.
pub struct HoverCardState {
    open: bool,
    open_delay: Duration,
    close_delay: Duration,
    on_open_change: Option<OpenChangeHandler>,
    open_task: Option<Task<()>>,
    close_task: Option<Task<()>>,
    epoch: usize,
    is_hovering_trigger: bool,
    is_hovering_content: bool,
    /// The release of a press a click-through card handed on, when it came
    /// before the press could be handed on.
    handed_on_release: Rc<RefCell<Option<gpui::MouseUpEvent>>>,
}

impl HoverCardState {
    fn new(open_delay: Duration, close_delay: Duration) -> Self {
        Self {
            open: false,
            open_delay,
            close_delay,
            on_open_change: None,
            open_task: None,
            close_task: None,
            epoch: 0,
            is_hovering_trigger: false,
            is_hovering_content: false,
            handed_on_release: Rc::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Close the card now — for a control inside it that has done its work.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_tasks();
        // The content goes with the card and cannot report the mouse leaving.
        self.is_hovering_content = false;
        self.set_open(false, window, cx);
    }

    fn sync(
        &mut self,
        open_delay: Duration,
        close_delay: Duration,
        on_open_change: Option<OpenChangeHandler>,
    ) {
        self.open_delay = open_delay;
        self.close_delay = close_delay;
        self.on_open_change = on_open_change;
    }

    fn schedule_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_tasks();
        let epoch = self.next_epoch();
        let delay = self.open_delay;
        self.open_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update_in(cx, |state, window, cx| {
                if state.epoch == epoch {
                    state.set_open(true, window, cx);
                }
            });
        }));
    }

    fn schedule_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_tasks();
        let epoch = self.next_epoch();
        let delay = self.close_delay;
        self.close_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update_in(cx, |state, window, cx| {
                if state.epoch == epoch && !state.is_hovering_trigger && !state.is_hovering_content
                {
                    state.set_open(false, window, cx);
                }
            });
        }));
    }

    fn cancel_tasks(&mut self) {
        self.epoch += 1;
        self.open_task = None;
        self.close_task = None;
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.open == open {
            return;
        }

        self.open = open;
        cx.notify();
        // The change is announced from here rather than from the element,
        // because the delay timers outlive the `HoverCard` that carried the
        // handler: by the time the state flips, that element is long gone.
        if let Some(on_open_change) = self.on_open_change.clone() {
            on_open_change(&open, window, cx);
        }
    }

    fn on_trigger_hover(&mut self, hovering: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.is_hovering_trigger = hovering;
        if hovering {
            self.schedule_open(window, cx);
        } else if !self.is_hovering_content {
            self.schedule_close(window, cx);
        }
    }

    fn on_content_hover(&mut self, hovering: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.is_hovering_content = hovering;
        if hovering {
            self.cancel_tasks();
        } else if !self.is_hovering_trigger {
            self.schedule_close(window, cx);
        }
    }
}

impl Render for HoverCardState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RenderOnce for HoverCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| {
            HoverCardState::new(self.open_delay, self.close_delay)
        });
        state.update(cx, |state, _| {
            state.sync(self.open_delay, self.close_delay, self.on_open_change)
        });
        let open = state.read(cx).is_open();

        let trigger = self.trigger.unwrap_or_else(|| div().into_any_element());
        let popup = Popup::new(
            self.id,
            div()
                .id("trigger")
                .child(trigger)
                .when(self.tap_to_open, |trigger| {
                    trigger.on_click(window.listener_for(&state, move |state, _, window, cx| {
                        state.cancel_tasks();
                        // Toggle the state rendered by this trigger even if
                        // outside dismissal handles the same release first.
                        state.set_open(!open, window, cx);
                    }))
                })
                .when(!self.tap_to_open, |trigger| {
                    trigger.on_hover(window.listener_for(&state, |state, hovered, window, cx| {
                        state.on_trigger_hover(*hovered, window, cx)
                    }))
                }),
        )
        .refine_style(&self.style)
        .anchor(self.anchor)
        .content_offset(self.content_offset)
        .mouse_transparent(!self.interactive);

        if !open {
            return popup;
        }

        popup.when_some(self.content, |popup, content| {
            let hover = window.listener_for(&state, |state, hovered, window, cx| {
                state.on_content_hover(*hovered, window, cx)
            });
            let dismiss = window.listener_for(&state, |state, _, window, cx| {
                state.cancel_tasks();
                state.set_open(false, window, cx);
            });
            let give_away = window.listener_for(
                &state,
                |state: &mut HoverCardState, event: &gpui::MouseDownEvent, window, cx| {
                    state.close(window, cx);
                    // The press was tested against a frame with the card in
                    // it. Once this event is through, paint the frame without
                    // the card and give the press to what is there now — in
                    // this turn of the loop, so that the release that follows
                    // finds it pressed.
                    //
                    // A release that gets here before that (two events
                    // dispatched in one go) still lands on the card, which
                    // notes it; it is handed on behind the press. A press
                    // without its release would leave a row pressed, and the
                    // next mouse move would drag it.
                    let event = event.clone();
                    let release = state.handed_on_release.clone();
                    release.take();
                    window.defer(cx, move |window, cx| {
                        window.draw(cx).clear(cx);
                        window.dispatch_event(gpui::PlatformInput::MouseDown(event), cx);
                        if let Some(release) = release.take() {
                            window.dispatch_event(gpui::PlatformInput::MouseUp(release), cx);
                        }
                        // That draw was not presented.
                        window.refresh();
                    });
                },
            );
            let note_release = window.listener_for(
                &state,
                |state: &mut HoverCardState, event: &gpui::MouseUpEvent, _, _| {
                    state.handed_on_release.replace(Some(event.clone()));
                },
            );
            popup.content(state.update(cx, |state, cx| {
                content(state, window, cx)
                    .when(self.tap_to_open, |content| {
                        content.on_mouse_up_out(gpui::MouseButton::Left, dismiss)
                    })
                    .when(!self.tap_to_open && self.interactive, |content| {
                        content.on_hover(hover)
                    })
                    .when(
                        !self.tap_to_open && self.interactive && self.click_through,
                        |content| {
                            // Bubble phase: a control inside has had the press
                            // first and stopped it if it was its own.
                            content
                                .on_mouse_down(gpui::MouseButton::Left, give_away)
                                .on_mouse_up(gpui::MouseButton::Left, note_release)
                        },
                    )
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{Context, Render, TestAppContext, point, px};

    use super::*;

    #[derive(Default)]
    struct Harness {
        open_changes: Rc<RefCell<Vec<bool>>>,
        tap_to_open: bool,
        passive: bool,
        click_through: bool,
        clicks_below: Rc<Cell<usize>>,
        clicks_on_control: Rc<Cell<usize>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let delay = Duration::from_millis(100);
            let open_changes = self.open_changes.clone();
            let clicks_below = self.clicks_below.clone();
            let clicks_on_control = self.clicks_on_control.clone();
            // The card hangs below its trigger, over a strip that counts the
            // clicks it gets.
            div()
                .size_full()
                .child(
                    div()
                        .id("below")
                        .absolute()
                        .top(px(20.))
                        .left_0()
                        .w(px(60.))
                        .h(px(40.))
                        .on_click(move |_, _, _| clicks_below.set(clicks_below.get() + 1)),
                )
                .child(
                    HoverCard::new("hover-card")
                        .map(|mut card| {
                            card.tap_to_open = self.tap_to_open;
                            card
                        })
                        .interactive(!self.passive)
                        .click_through(self.click_through)
                        .anchor(Anchor::TopLeft)
                        .open_delay(delay)
                        .close_delay(delay)
                        .on_open_change(move |open, _, _| open_changes.borrow_mut().push(*open))
                        .trigger(
                            div()
                                .debug_selector(|| "hover-card-trigger".into())
                                .size(px(20.)),
                        )
                        .content(move |_, _, _| {
                            // A card with one control in its top left corner.
                            div()
                                .id("hover-card-content")
                                .debug_selector(|| "hover-card-content".into())
                                .size(px(10.))
                                .child(
                                    div()
                                        .id("control")
                                        .debug_selector(|| "hover-card-control".into())
                                        .size(px(4.))
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .on_click(move |_, _, _| {
                                            clicks_on_control.set(clicks_on_control.get() + 1)
                                        }),
                                )
                        }),
                )
        }
    }

    /// Open the card by hovering its trigger; returns the card's middle.
    fn open_card(delay: Duration, cx: &mut gpui::VisualTestContext) -> gpui::Point<Pixels> {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, Default::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        cx.debug_bounds("hover-card-content").unwrap().center()
    }

    /// The default: the mouse may move onto the card, which then stays and
    /// takes the click.
    #[gpui::test]
    fn an_interactive_card_stays_under_the_mouse_and_takes_the_click(cx: &mut TestAppContext) {
        let delay = Duration::from_millis(100);
        let clicks_below = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks_below = clicks_below.clone();
            move |_, _| Harness {
                clicks_below,
                ..Default::default()
            }
        });
        let on_card = open_card(delay, cx);
        cx.simulate_mouse_move(on_card, None, Default::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_some());

        cx.simulate_click(on_card, Default::default());
        assert_eq!(clicks_below.get(), 0);
    }

    /// A click-through card can be entered, and its control takes its click
    /// without the card closing; a click anywhere else on the card closes it
    /// and reaches what it covers.
    #[gpui::test]
    fn a_click_through_card_keeps_its_controls_and_gives_the_rest_away(cx: &mut TestAppContext) {
        let delay = Duration::from_millis(100);
        let clicks_below = Rc::new(Cell::new(0));
        let clicks_on_control = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks_below = clicks_below.clone();
            let clicks_on_control = clicks_on_control.clone();
            move |_, _| Harness {
                click_through: true,
                clicks_below,
                clicks_on_control,
                ..Default::default()
            }
        });
        let on_card = open_card(delay, cx);

        // Entered, it stays.
        cx.simulate_mouse_move(on_card, None, Default::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_some());

        let on_control = cx.debug_bounds("hover-card-control").unwrap().center();
        cx.simulate_mouse_move(on_control, None, Default::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(on_control, Default::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!((clicks_on_control.get(), clicks_below.get()), (1, 0));
        assert!(cx.debug_bounds("hover-card-content").is_some());

        // Beside the control: through to the strip, and the card is gone.
        let beside = on_card + point(px(3.), px(3.));
        cx.simulate_mouse_move(beside, None, Default::default());
        cx.simulate_click(beside, Default::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!((clicks_on_control.get(), clicks_below.get()), (1, 1));
        assert!(cx.debug_bounds("hover-card-content").is_none());
    }

    /// A card that only shows something closes when the trigger is left, even
    /// towards the card, and a click where it is reaches what it covers.
    #[gpui::test]
    fn a_passive_card_closes_off_the_trigger_and_lets_the_click_through(cx: &mut TestAppContext) {
        let delay = Duration::from_millis(100);
        let clicks_below = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks_below = clicks_below.clone();
            move |_, _| Harness {
                passive: true,
                clicks_below,
                ..Default::default()
            }
        });
        let on_card = open_card(delay, cx);

        // While it is still open, the click goes through it.
        cx.simulate_mouse_move(on_card, None, Default::default());
        cx.simulate_click(on_card, Default::default());
        assert_eq!(clicks_below.get(), 1);

        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_none());
    }

    #[gpui::test]
    fn public_hover_card_owns_delayed_open_and_close(cx: &mut TestAppContext) {
        let delay = Duration::from_millis(100);
        let (_, cx) = cx.add_window_view(|_, _| Harness::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_mouse_move(point(px(10.), px(10.)), None, gpui::Modifiers::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("hover-card-content").is_some());

        cx.simulate_mouse_move(point(px(100.), px(100.)), None, gpui::Modifiers::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_none());
    }

    #[gpui::test]
    fn public_hover_card_reports_each_open_change(cx: &mut TestAppContext) {
        let delay = Duration::from_millis(100);
        let open_changes = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let open_changes = open_changes.clone();
            move |_, _| Harness {
                open_changes,
                ..Default::default()
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.simulate_mouse_move(point(px(10.), px(10.)), None, gpui::Modifiers::default());
        assert_eq!(*open_changes.borrow(), Vec::<bool>::new());

        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        assert_eq!(*open_changes.borrow(), vec![true]);

        cx.simulate_mouse_move(point(px(100.), px(100.)), None, gpui::Modifiers::default());
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(*open_changes.borrow(), vec![true, false]);
    }
    #[gpui::test]
    fn tap_card_ignores_hover_and_toggles_and_dismisses(cx: &mut TestAppContext) {
        let changes = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let changes = changes.clone();
            move |_, _| Harness {
                tap_to_open: true,
                open_changes: changes,
                ..Default::default()
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, Default::default());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(changes.borrow().is_empty());

        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("hover-card-content").is_some());
        cx.simulate_mouse_move(point(px(100.), px(100.)), None, Default::default());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(*changes.borrow(), vec![true]);

        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_none());
        assert_eq!(*changes.borrow(), vec![true, false]);

        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        let bounds = cx.debug_bounds("hover-card-content").unwrap();
        cx.simulate_click(bounds.center(), Default::default());
        assert_eq!(*changes.borrow(), vec![true, false, true]);
        cx.simulate_click(point(px(100.), px(100.)), Default::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hover-card-content").is_none());
        assert_eq!(*changes.borrow(), vec![true, false, true, false]);
    }
}
