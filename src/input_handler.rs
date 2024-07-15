use std::{convert::TryInto, process::Command, sync::atomic::Ordering};

use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    AnvilState,
};

#[cfg(feature = "udev")]
use crate::udev::UdevData;
#[cfg(feature = "udev")]
use smithay::backend::renderer::DebugFlags;

use smithay::{
    backend::input::{
        self, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent,
    },
    desktop::Window,
    input::{
        keyboard::{keysyms as xkb, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    output::Output,
    reexports::wayland_server::protocol::wl_pointer,
    utils::{Logical, Point, Serial, SERIAL_COUNTER as SCOUNTER},
    wayland::input_method::InputMethodSeat,
};

#[cfg(any(feature = "winit", feature = "x11", feature = "udev"))]
use smithay::backend::input::AbsolutePositionEvent;

#[cfg(any(feature = "winit", feature = "x11"))]
use smithay::output::Output;
use tracing::{debug, error, info};

use crate::state::Backend;
#[cfg(feature = "udev")]
use smithay::{
    backend::{
        input::{
            Device, DeviceCapability, GestureBeginEvent, GestureEndEvent,
            GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _, PointerMotionEvent,
            TabletToolEvent, TouchEvent,
        },
        session::Session,
    },
    input::{
        pointer::{
            GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
            GestureSwipeEndEvent, GestureSwipeUpdateEvent, RelativeMotionEvent,
        },
        touch::{DownEvent, UpEvent},
    },
    reexports::wayland_server::DisplayHandle,
    wayland::{
        pointer_constraints::{with_pointer_constraint, PointerConstraint},
        seat::WaylandFocus,
        tablet_manager::{TabletDescriptor, TabletSeatTrait},
    },
};

impl<BackendData: Backend> AnvilState<BackendData> {
    fn process_common_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::None => (),

            KeyAction::Quit => {
                info!("Quitting.");
                self.running.store(false, Ordering::SeqCst);
            }

            KeyAction::Run(cmd) => {
                info!(cmd, "Starting program");

                if let Err(e) = Command::new(&cmd)
                    .envs(
                        self.socket_name
                            .clone()
                            .map(|v| ("WAYLAND_DISPLAY", v))
                            .into_iter()
                            .chain(
                                #[cfg(feature = "xwayland")]
                                self.xdisplay.map(|v| ("DISPLAY", format!(":{}", v))),
                                #[cfg(not(feature = "xwayland"))]
                                None,
                            ),
                    )
                    .spawn()
                {
                    error!(cmd, err = %e, "Failed to start program");
                }
            }

            _ => unreachable!(
                "Common key action handler encountered backend specific action {:?}",
                action
            ),
        }
    }

    fn keyboard_key_to_action<B: InputBackend>(&mut self, evt: B::KeyboardKeyEvent) -> KeyAction {
        let keycode = evt.key_code();
        let state = evt.state();
        debug!(keycode, ?state, "key");
        let serial = SCOUNTER.next_serial();
        let time = Event::time_msec(&evt);
        let mut suppressed_keys = self.suppressed_keys.clone();
        let keyboard = self.seat.get_keyboard().unwrap();

        let inhibited = false;

        let action = keyboard
            .input(
                self,
                keycode,
                state,
                serial,
                time,
                |_, modifiers, handle| {
                    let keysym = handle.modified_sym();

                    debug!(
                        ?state,
                        mods = ?modifiers,
                        keysym = ::xkbcommon::xkb::keysym_get_name(keysym),
                        "keysym"
                    );

                    // If the key is pressed and triggered a action
                    // we will not forward the key to the client.
                    // Additionally add the key to the suppressed keys
                    // so that we can decide on a release if the key
                    // should be forwarded to the client or not.
                    if let KeyState::Pressed = state {
                        if !inhibited {
                            let action = process_keyboard_shortcut(*modifiers, keysym);

                            if action.is_some() {
                                suppressed_keys.push(keysym);
                            }

                            action
                                .map(FilterResult::Intercept)
                                .unwrap_or(FilterResult::Forward)
                        } else {
                            FilterResult::Forward
                        }
                    } else {
                        let suppressed = suppressed_keys.contains(&keysym);
                        if suppressed {
                            suppressed_keys.retain(|k| *k != keysym);
                            FilterResult::Intercept(KeyAction::None)
                        } else {
                            FilterResult::Forward
                        }
                    }
                },
            )
            .unwrap_or(KeyAction::None);

        self.suppressed_keys = suppressed_keys;
        action
    }

    fn on_pointer_button<B: InputBackend>(&mut self, evt: B::PointerButtonEvent) {
        let serial = SCOUNTER.next_serial();
        let button = evt.button_code();

        let state = wl_pointer::ButtonState::from(evt.state());

        if wl_pointer::ButtonState::Pressed == state {
            self.update_keyboard_focus(serial);
        };
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: state.try_into().unwrap(),
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn update_keyboard_focus(&mut self, serial: Serial) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let touch = self.seat.get_touch();
        let input_method = self.seat.input_method();
        if !self.pointer.is_grabbed()
            && (!keyboard.is_grabbed() || input_method.keyboard_grabbed())
            && !touch.map(|touch| touch.is_grabbed()).unwrap_or(false)
        {
            let kbd = match self.elements.get(0) {
                Some(window) => Some(KeyboardFocusTarget::from(window.clone())),
                None => None,
            };
            keyboard.set_focus(self, kbd, serial);
        }
    }

    pub fn current_window(&self) -> Option<Window> {
        if let Some(win) = self.elements.get(0) {
            return Some(win.clone());
        }
        None
    }

    pub fn current_window_with_origin(&self) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        match self.current_window() {
            Some(window) => Some((window.into(), Point::from((0f64, 0f64)))),
            None => None,
        }
    }

    pub fn current_window_with_output_pointer_location(
        &self,
        _point: Point<f64, Logical>,
        _output: &Output,
    ) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        unimplemented!()
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, evt: B::PointerAxisEvent) {
        let horizontal_amount = evt.amount(input::Axis::Horizontal).unwrap_or_else(|| {
            evt.amount_v120(input::Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
        });
        let vertical_amount = evt
            .amount(input::Axis::Vertical)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
        let horizontal_amount_discrete = evt.amount_v120(input::Axis::Horizontal);
        let vertical_amount_discrete = evt.amount_v120(input::Axis::Vertical);

        {
            let mut frame = AxisFrame::new(evt.time_msec()).source(evt.source());
            if horizontal_amount != 0.0 {
                frame = frame
                    .relative_direction(Axis::Horizontal, evt.relative_direction(Axis::Horizontal));
                frame = frame.value(Axis::Horizontal, horizontal_amount);
                if let Some(discrete) = horizontal_amount_discrete {
                    frame = frame.v120(Axis::Horizontal, discrete as i32);
                }
            }
            if vertical_amount != 0.0 {
                frame = frame
                    .relative_direction(Axis::Vertical, evt.relative_direction(Axis::Vertical));
                frame = frame.value(Axis::Vertical, vertical_amount);
                if let Some(discrete) = vertical_amount_discrete {
                    frame = frame.v120(Axis::Vertical, discrete as i32);
                }
            }
            if evt.source() == AxisSource::Finger {
                if evt.amount(Axis::Horizontal) == Some(0.0) {
                    frame = frame.stop(Axis::Horizontal);
                }
                if evt.amount(Axis::Vertical) == Some(0.0) {
                    frame = frame.stop(Axis::Vertical);
                }
            }
            let pointer = self.pointer.clone();
            pointer.axis(self, frame);
            pointer.frame(self);
        }
    }
}

#[cfg(any(feature = "winit", feature = "x11"))]
impl<BackendData: Backend> AnvilState<BackendData> {
    pub fn process_input_event_windowed<B: InputBackend>(
        &mut self,
        event: InputEvent<B>,
        output_name: &str,
    ) {
        match event {
            InputEvent::Keyboard { event } => match self.keyboard_key_to_action::<B>(event) {
                KeyAction::ScaleUp => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = current_scale + 0.25;
                    output.change_current_state(
                        None,
                        None,
                        Some(Scale::Fractional(new_scale)),
                        None,
                    );

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::ScaleDown => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = f64::max(1.0, current_scale - 0.25);
                    output.change_current_state(
                        None,
                        None,
                        Some(Scale::Fractional(new_scale)),
                        None,
                    );

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::RotateOutput => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_transform = output.current_transform();
                    let new_transform = match current_transform {
                        Transform::Normal => Transform::_90,
                        Transform::_90 => Transform::_180,
                        Transform::_180 => Transform::_270,
                        Transform::_270 => Transform::Flipped,
                        Transform::Flipped => Transform::Flipped90,
                        Transform::Flipped90 => Transform::Flipped180,
                        Transform::Flipped180 => Transform::Flipped270,
                        Transform::Flipped270 => Transform::Normal,
                    };
                    tracing::info!(?current_transform, ?new_transform, output = ?output.name(), "changing output transform");
                    output.change_current_state(None, Some(new_transform), None, None);
                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::Quit
                    | KeyAction::Run(_)
                    | KeyAction::ToggleMenu
                    | KeyAction::ArrowUp
                    | KeyAction::ArrowDown
                    | KeyAction::ArrowLeft
                    | KeyAction::ArrowRight
                    | KeyAction::Select
                    | KeyAction::Back => self.process_common_key_action(action),

                    _ => tracing::warn!(
                        ?action,
                        output_name,
                        "Key action unsupported on on output backend.",
                    ),
                },
            },

            InputEvent::PointerMotionAbsolute { event } => {
                let output = self
                    .space
                    .outputs()
                    .find(|o| o.name() == output_name)
                    .unwrap()
                    .clone();
                self.on_pointer_move_absolute_windowed::<B>(event, &output)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            _ => (), // other events are not handled in anvil (yet)
        }
    }

    fn on_pointer_move_absolute_windowed<B: InputBackend>(
        &mut self,
        evt: B::PointerMotionAbsoluteEvent,
        output: &Output,
    ) {
        println!("Pointe absolute windowed");
        let output_geo = self.space.output_geometry(output).unwrap();

        let pos = evt.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    pub fn release_all_keys(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for keycode in keyboard.pressed_keys() {
            keyboard.input(
                self,
                keycode.raw(),
                KeyState::Released,
                SCOUNTER.next_serial(),
                0,
                |_, _, _| FilterResult::Forward::<bool>,
            );
        }
    }
}

#[cfg(feature = "udev")]
impl AnvilState<UdevData> {
    pub fn process_input_event<B: InputBackend>(
        &mut self,
        dh: &DisplayHandle,
        event: InputEvent<B>,
    ) {
        match event {
            InputEvent::Keyboard { event, .. } => match self.keyboard_key_to_action::<B>(event) {
                #[cfg(feature = "udev")]
                KeyAction::VtSwitch(vt) => {
                    info!(to = vt, "Trying to switch vt");
                    if let Err(err) = self.backend_data.session.change_vt(vt) {
                        error!(vt, "Error switching vt: {}", err);
                    }
                }
                KeyAction::ToggleTint => {
                    let mut debug_flags = self.backend_data.debug_flags();
                    debug_flags.toggle(DebugFlags::TINT);
                    self.backend_data.set_debug_flags(debug_flags);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::Quit
                    | KeyAction::Run(_)
                    | KeyAction::ArrowUp
                    | KeyAction::ArrowDown
                    | KeyAction::ArrowLeft
                    | KeyAction::ArrowRight
                    | KeyAction::Select
                    | KeyAction::Back => self.process_common_key_action(action),

                    _ => unreachable!(),
                },
            },
            InputEvent::PointerMotion { event, .. } => self.on_pointer_move::<B>(dh, event),
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_pointer_move_absolute::<B>(dh, event)
            }
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<B>(event),
            InputEvent::TabletToolAxis { event, .. } => self.on_tablet_tool_axis::<B>(event),
            InputEvent::TabletToolProximity { event, .. } => {
                self.on_tablet_tool_proximity::<B>(dh, event)
            }
            InputEvent::TabletToolTip { event, .. } => self.on_tablet_tool_tip::<B>(event),
            InputEvent::TabletToolButton { event, .. } => self.on_tablet_button::<B>(event),
            InputEvent::GestureSwipeBegin { event, .. } => self.on_gesture_swipe_begin::<B>(event),
            InputEvent::GestureSwipeUpdate { event, .. } => {
                self.on_gesture_swipe_update::<B>(event)
            }
            InputEvent::GestureSwipeEnd { event, .. } => self.on_gesture_swipe_end::<B>(event),
            InputEvent::GesturePinchBegin { event, .. } => self.on_gesture_pinch_begin::<B>(event),
            InputEvent::GesturePinchUpdate { event, .. } => {
                self.on_gesture_pinch_update::<B>(event)
            }
            InputEvent::GesturePinchEnd { event, .. } => self.on_gesture_pinch_end::<B>(event),
            InputEvent::GestureHoldBegin { event, .. } => self.on_gesture_hold_begin::<B>(event),
            InputEvent::GestureHoldEnd { event, .. } => self.on_gesture_hold_end::<B>(event),

            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchFrame { event } => self.on_touch_frame::<B>(event),
            InputEvent::TouchCancel { event } => self.on_touch_cancel::<B>(event),

            InputEvent::DeviceAdded { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    self.seat
                        .tablet_seat()
                        .add_tablet::<Self>(dh, &TabletDescriptor::from(&device));
                }
                if device.has_capability(DeviceCapability::Touch) && self.seat.get_touch().is_none()
                {
                    self.seat.add_touch();
                }
            }
            InputEvent::DeviceRemoved { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    let tablet_seat = self.seat.tablet_seat();

                    tablet_seat.remove_tablet(&TabletDescriptor::from(&device));

                    // If there are no tablets in seat we can remove all tools
                    if tablet_seat.count_tablets() == 0 {
                        tablet_seat.clear_tools();
                    }
                }
            }
            _ => {
                // other events are not handled in anvil (yet)
            }
        }
    }

    fn on_pointer_move<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionEvent,
    ) {
        let mut pointer_location = self.pointer.current_location();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();

        let mut pointer_locked = false;

        if let Some(window) = self.current_window() {
            if let Some(surface) = window.wl_surface() {
                with_pointer_constraint(&surface, &pointer, |constraint| match constraint {
                    Some(constraint) if constraint.is_active() => match &*constraint {
                        PointerConstraint::Locked(_locked) => {
                            pointer_locked = true;
                        }
                        _ => {}
                    },
                    _ => {}
                });
            }

            pointer.relative_motion(
                self,
                self.current_window_with_origin(),
                &RelativeMotionEvent {
                    delta: evt.delta(),
                    delta_unaccel: evt.delta_unaccel(),
                    utime: evt.time(),
                },
            );
        }

        // If pointer is locked, only emit relative motion
        if pointer_locked {
            pointer.frame(self);
            return;
        }

        pointer_location += evt.delta();

        // clamp to screen limits
        // this event is never generated by winit
        pointer_location = self.clamp_coords(pointer_location);

        pointer.motion(
            self,
            self.current_window_with_origin(),
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_move_absolute<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionAbsoluteEvent,
    ) {
        /* WARNING This assumes a position in window space... between 0,0 and width,height
        If the value is in output space it should be shifted & scaled */
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();

        if let Some(window) = self.current_window() {
            let max_x = window.bbox().size.w;
            let max_y = window.bbox().size.h;

            let mut pointer_location = (evt.x_transformed(max_x), evt.y_transformed(max_y)).into();

            // clamp to screen limits
            pointer_location = self.clamp_coords(pointer_location);

            pointer.motion(
                self,
                Some((
                    PointerFocusTarget::from(window.wl_surface().unwrap().into_owned()),
                    pointer_location,
                )),
                &MotionEvent {
                    location: pointer_location,
                    serial,
                    time: evt.time_msec(),
                },
            );
        }

        pointer.frame(self);
    }

    fn on_tablet_tool_axis<B: InputBackend>(&mut self, _evt: B::TabletToolAxisEvent) {
        let _tablet_seat = self.seat.tablet_seat();
        unimplemented!();
        /*
        let output_geometry = self
            .space
            .outputs()
            .next()
            .map(|o| self.space.output_geometry(o).unwrap());

        if let Some(rect) = output_geometry {
            let pointer_location = evt.position_transformed(rect.size) + rect.loc.to_f64();

            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&evt.tool());

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: 0,
                },
            );

            if let (Some(tablet), Some(tool)) = (tablet, tool) {
                if evt.pressure_has_changed() {
                    tool.pressure(evt.pressure());
                }
                if evt.distance_has_changed() {
                    tool.distance(evt.distance());
                }
                if evt.tilt_has_changed() {
                    tool.tilt(evt.tilt());
                }
                if evt.slider_has_changed() {
                    tool.slider_position(evt.slider_position());
                }
                if evt.rotation_has_changed() {
                    tool.rotation(evt.rotation());
                }
                if evt.wheel_has_changed() {
                    tool.wheel(evt.wheel_delta(), evt.wheel_delta_discrete());
                }

                tool.motion(
                    pointer_location,
                    under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                    &tablet,
                    SCOUNTER.next_serial(),
                    evt.time_msec(),
                );
            }

            pointer.frame(self);
        }
        */
    }

    fn on_tablet_tool_proximity<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        _evt: B::TabletToolProximityEvent,
    ) {
        let _tablet_seat = self.seat.tablet_seat();
        unimplemented!();
        /*
        let output_geometry = self
            .space
            .outputs()
            .next()
            .map(|o| self.space.output_geometry(o).unwrap());

        if let Some(rect) = output_geometry {
            let tool = evt.tool();
            tablet_seat.add_tool::<Self>(dh, &tool);

            let pointer_location = evt.position_transformed(rect.size) + rect.loc.to_f64();

            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&tool);

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: 0,
                },
            );
            pointer.frame(self);

            if let (Some(under), Some(tablet), Some(tool)) = (
                under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                tablet,
                tool,
            ) {
                match evt.state() {
                    ProximityState::In => tool.proximity_in(
                        pointer_location,
                        under,
                        &tablet,
                        SCOUNTER.next_serial(),
                        evt.time_msec(),
                    ),
                    ProximityState::Out => tool.proximity_out(evt.time_msec()),
                }
            }
        }
        */
    }

    fn on_tablet_tool_tip<B: InputBackend>(&mut self, evt: B::TabletToolTipEvent) {
        let _tool = self.seat.tablet_seat().get_tool(&evt.tool());
        unimplemented!();
        /*
        if let Some(tool) = tool {
            match evt.tip_state() {
                TabletToolTipState::Down => {
                    let serial = SCOUNTER.next_serial();
                    tool.tip_down(serial, evt.time_msec());

                    // change the keyboard focus
                    self.update_keyboard_focus(self.pointer.current_location(), serial);
                }
                TabletToolTipState::Up => {
                    tool.tip_up(evt.time_msec());
                }
            }
        }
        */
    }

    fn on_tablet_button<B: InputBackend>(&mut self, evt: B::TabletToolButtonEvent) {
        let _tool = self.seat.tablet_seat().get_tool(&evt.tool());
        unimplemented!();
        /*
        if let Some(tool) = tool {
            tool.button(
                evt.button(),
                evt.button_state(),
                SCOUNTER.next_serial(),
                evt.time_msec(),
            );
        }
        */
    }

    fn on_gesture_swipe_begin<B: InputBackend>(&mut self, evt: B::GestureSwipeBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_swipe_update<B: InputBackend>(&mut self, evt: B::GestureSwipeUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
            },
        );
    }

    fn on_gesture_swipe_end<B: InputBackend>(&mut self, evt: B::GestureSwipeEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_pinch_begin<B: InputBackend>(&mut self, evt: B::GesturePinchBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_pinch_update<B: InputBackend>(&mut self, evt: B::GesturePinchUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
                scale: evt.scale(),
                rotation: evt.rotation(),
            },
        );
    }

    fn on_gesture_pinch_end<B: InputBackend>(&mut self, evt: B::GesturePinchEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_hold_begin<B: InputBackend>(&mut self, evt: B::GestureHoldBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_hold_end<B: InputBackend>(&mut self, evt: B::GestureHoldEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn touch_location_transformed<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        evt: &E,
    ) -> Option<Point<f64, Logical>> {
        let output = self
            .outputs
            .iter()
            .find(|output| output.name().starts_with("eDP"))
            .or_else(|| self.outputs.iter().next());

        let output = output?;

        let geometry = output.current_mode().unwrap();

        let transform = output.current_transform();
        let logical_size: smithay::utils::Size<i32, smithay::utils::Logical> =
            geometry.size.to_logical(1);
        let size = transform.invert().transform_size(logical_size);
        Some(transform.transform_point_in(evt.position_transformed(size), &size.to_f64()))
    }

    fn on_touch_down<B: InputBackend>(&mut self, evt: B::TouchDownEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };

        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let serial = SCOUNTER.next_serial();
        self.update_keyboard_focus(serial);

        //let under = self.surface_under(touch_location);

        let touch_surface = self.current_window_with_origin();
        handle.down(
            self,
            touch_surface,
            &DownEvent {
                slot: evt.slot(),
                location: touch_location,
                serial,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_up<B: InputBackend>(&mut self, evt: B::TouchUpEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let serial = SCOUNTER.next_serial();
        handle.up(
            self,
            &UpEvent {
                slot: evt.slot(),
                serial,
                time: evt.time_msec(),
            },
        )
    }
    fn on_touch_motion<B: InputBackend>(&mut self, evt: B::TouchMotionEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let touch_surface = self.current_window_with_origin();
        handle.motion(
            self,
            touch_surface,
            &smithay::input::touch::MotionEvent {
                slot: evt.slot(),
                location: touch_location,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_frame<B: InputBackend>(&mut self, _evt: B::TouchFrameEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.frame(self);
    }
    fn on_touch_cancel<B: InputBackend>(&mut self, _evt: B::TouchCancelEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.cancel(self);
    }

    fn clamp_coords(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        if let Some(window) = self.current_window() {
            let (pos_x, pos_y) = pos.into();
            let max_x = window.bbox().size.w;
            let max_y = window.bbox().size.h;

            let clamped_x = pos_x.clamp(0.0, max_x as f64);
            let clamped_y = pos_y.clamp(0.0, max_y as f64);
            return (clamped_x, clamped_y).into();
        }
        return pos;
    }
}

/// Possible results of a keyboard action
#[allow(dead_code)] // some of these are only read if udev is enabled
#[derive(Debug)]
enum KeyAction {
    /// Quit the compositor
    Quit,
    /// Trigger a vt-switch
    VtSwitch(i32),
    /// run a command
    Run(String),
    ToggleTint,
    ArrowDown,
    ArrowUp,
    ArrowLeft,
    ArrowRight,
    Select,
    Back,
    /// Do nothing more
    None,
}

fn process_keyboard_shortcut(modifiers: ModifiersState, keysym: Keysym) -> Option<KeyAction> {
    if modifiers.ctrl && modifiers.alt && keysym == Keysym::BackSpace
        || modifiers.logo && keysym == Keysym::q
    {
        // ctrl+alt+backspace = quit
        // logo + q = quit
        Some(KeyAction::Quit)
    } else if (xkb::KEY_XF86Switch_VT_1..=xkb::KEY_XF86Switch_VT_12).contains(&keysym.raw()) {
        // VTSwitch
        Some(KeyAction::VtSwitch(
            (keysym.raw() - xkb::KEY_XF86Switch_VT_1 + 1) as i32,
        ))
    } else if modifiers.logo && keysym == Keysym::Return {
        // run terminal
        Some(KeyAction::Run("xfce4-terminal".into()))
    } else if keysym == Keysym::Alt_R || keysym == Keysym::Menu {
        Some(KeyAction::Run("consolation-menu".into()))
    } else if modifiers.logo && modifiers.shift && keysym == Keysym::T {
        Some(KeyAction::ToggleTint)
    } else {
        None
    }
}
