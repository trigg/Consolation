use arrayvec::ArrayVec;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_protocols_wlr;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{
    ToplevelStateSet, XdgToplevelSurfaceData, XdgToplevelSurfaceRoleAttributes,
};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;
use zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1;

use crate::state::{AnvilState, Backend};

const VERSION: u32 = 3;

#[derive(Debug)]
pub struct ForeignToplevelManagerState {
    display: DisplayHandle,
    instances: Vec<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<WlSurface, ToplevelData>,
}

pub trait ForeignToplevelHandler {
    fn foreign_toplevel_manager_state(&mut self) -> &mut ForeignToplevelManagerState;
    fn activate(&mut self, wl_surface: WlSurface);
    fn close(&mut self, wl_surface: WlSurface);
    fn set_fullscreen(&mut self, wl_surface: WlSurface, wl_output: Option<WlOutput>);
    fn unset_fullscreen(&mut self, wl_surface: WlSurface);
    fn set_maximized(&mut self, wl_surface: WlSurface);
    fn unset_maximized(&mut self, wl_surface: WlSurface);
    fn set_minimized(&mut self, wl_surface: WlSurface);
    fn unset_minimized(&mut self, wl_surface: WlSurface);
}

#[derive(Debug)]
struct ToplevelData {
    title: Option<String>,
    app_id: Option<String>,
    states: ArrayVec<u32, 3>,
    output: Option<Output>,
    instances: HashMap<ZwlrForeignToplevelHandleV1, Vec<WlOutput>>,
}

pub struct ForeignToplevelGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

impl ForeignToplevelManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData>,
        D: Dispatch<ZwlrForeignToplevelManagerV1, ()>,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ForeignToplevelGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, ZwlrForeignToplevelManagerV1, _>(VERSION, global_data);
        Self {
            display: display.clone(),
            instances: Vec::new(),
            toplevels: HashMap::new(),
        }
    }
}

pub fn refresh<D>(state: &mut AnvilState<D>)
where
    D: Backend + 'static,
{
    let protocol_state = &mut state.toplevel_manager;

    // Handle closed windows.
    protocol_state.toplevels.retain(|surface, data| {
        if state
            .elements
            .iter()
            .find(|window| match window.wl_surface() {
                Some(window_surface) => window_surface.id() == surface.id(),
                None => false,
            })
            .is_some()
        {
            return true;
        }

        tracing::info!("Removing window");
        for instance in data.instances.keys() {
            instance.closed();
        }

        false
    });

    let mut focus = true;
    state.elements.iter().for_each(|mapped| {
        if let Some(wl_surface) = mapped.wl_surface() {
            let wl_surface = wl_surface.into_owned();

            if mapped.is_x11() {
                let xwindow = mapped.x11_surface().unwrap();
                let title = Some(xwindow.title());
                let app_id = xwindow.startup_id();
                let maximized = xwindow.is_maximized();
                let minimized = xwindow.is_minimized();
                let fullscreen = xwindow.is_fullscreen();
                let output = state.outputs.get(0);
                refresh_toplevel_x11::<D>(
                    protocol_state,
                    &wl_surface,
                    title,
                    app_id,
                    maximized,
                    minimized,
                    fullscreen,
                    output,
                    focus,
                );
            } else {
                with_states(&wl_surface, |states| {
                    let role = states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap();
                    let output = state.outputs.get(0);
                    focus =
                        refresh_toplevel::<D>(protocol_state, &wl_surface, &role, output, focus);
                });
            }
        }
        focus = false;
    });
}

fn refresh_toplevel_x11<D>(
    protocol_state: &mut ForeignToplevelManagerState,
    wl_surface: &WlSurface,
    title: Option<String>,
    app_id: Option<String>,
    maximized: bool,
    minimized: bool,
    fullscreen: bool,
    output: Option<&Output>,
    has_focus: bool,
) where
    D: Backend + 'static,
{
    let mut states: ArrayVec<u32, 3> = ArrayVec::new();
    if maximized {
        states.push(zwlr_foreign_toplevel_handle_v1::State::Maximized as u32);
    }
    if minimized {
        states.push(zwlr_foreign_toplevel_handle_v1::State::Minimized as u32);
    }
    if has_focus {
        states.push(zwlr_foreign_toplevel_handle_v1::State::Activated as u32);
    }
    if fullscreen {
        states.push(zwlr_foreign_toplevel_handle_v1::State::Fullscreen as u32);
    }
    let states = ArrayVec::new();
    match protocol_state.toplevels.entry(wl_surface.clone()) {
        Entry::Occupied(entry) => {
            let data = entry.into_mut();

            let mut new_title = None;
            if data.title != title && new_title.is_some() {
                data.title.clone_from(&title);
                new_title = title.as_deref();
            }

            let mut states_changed = false;
            if data.states != states {
                data.states = states;
                states_changed = true;
            }

            let mut output_changed = false;
            if data.output.as_ref() != output {
                data.output = output.cloned();
                output_changed = true;
            }

            let something_changed = new_title.is_some() || states_changed || output_changed;

            if something_changed {
                for (instance, outputs) in &mut data.instances {
                    if let Some(new_title) = new_title {
                        instance.title(new_title.to_owned());
                    }
                    if states_changed {
                        instance.state(data.states.iter().flat_map(|x| x.to_ne_bytes()).collect());
                    }
                    if output_changed {
                        for wl_output in outputs.drain(..) {
                            instance.output_leave(&wl_output);
                        }
                        if let Some(output) = &data.output {
                            if let Some(client) = instance.client() {
                                for wl_output in output.client_outputs(&client) {
                                    instance.output_enter(&wl_output);
                                    outputs.push(wl_output);
                                }
                            }
                        }
                    }
                    instance.done();
                }
            }

            for outputs in data.instances.values_mut() {
                // Clean up dead wl_outputs.
                outputs.retain(|x| x.is_alive());
            }
        }
        Entry::Vacant(entry) => {
            // New window, start tracking it.
            let mut data = ToplevelData {
                title: title.clone(),
                app_id: app_id.clone(),
                states,
                output: output.cloned(),
                instances: HashMap::new(),
            };

            for manager in &protocol_state.instances {
                if let Some(client) = manager.client() {
                    data.add_instance::<AnvilState<D>>(&protocol_state.display, &client, manager);
                }
            }

            entry.insert(data);
        }
    }
}

fn refresh_toplevel<D>(
    protocol_state: &mut ForeignToplevelManagerState,
    wl_surface: &WlSurface,
    role: &XdgToplevelSurfaceRoleAttributes,
    output: Option<&Output>,
    has_focus: bool,
) -> bool
where
    D: Backend + 'static,
{
    let mut has_focus = has_focus;

    let states = to_state_vec(&role.current.states, has_focus);
    if role.title.is_none() || role.title.clone().unwrap() != "nil" {
        has_focus = false;
    }
    match protocol_state.toplevels.entry(wl_surface.clone()) {
        Entry::Occupied(entry) => {
            // Existing window, check if anything changed.
            let data = entry.into_mut();

            let mut new_title = None;
            if data.title != role.title {
                data.title.clone_from(&role.title);
                new_title = role.title.as_deref();

                if new_title.is_none() {
                    tracing::error!("toplevel title changed to None");
                }
            }

            let mut new_app_id = None;
            if data.app_id != role.app_id {
                data.app_id.clone_from(&role.app_id);
                new_app_id = role.app_id.as_deref();

                if new_app_id.is_none() {
                    tracing::error!("toplevel app_id changed to None");
                }
            }

            let mut states_changed = false;
            if data.states != states {
                data.states = states;
                states_changed = true;
            }

            let mut output_changed = false;
            if data.output.as_ref() != output {
                data.output = output.cloned();
                output_changed = true;
            }

            let something_changed =
                new_title.is_some() || new_app_id.is_some() || states_changed || output_changed;

            if something_changed {
                for (instance, outputs) in &mut data.instances {
                    if let Some(new_title) = new_title {
                        instance.title(new_title.to_owned());
                    }
                    if let Some(new_app_id) = new_app_id {
                        instance.app_id(new_app_id.to_owned());
                    }
                    if states_changed {
                        instance.state(data.states.iter().flat_map(|x| x.to_ne_bytes()).collect());
                    }
                    if output_changed {
                        for wl_output in outputs.drain(..) {
                            instance.output_leave(&wl_output);
                        }
                        if let Some(output) = &data.output {
                            if let Some(client) = instance.client() {
                                for wl_output in output.client_outputs(&client) {
                                    instance.output_enter(&wl_output);
                                    outputs.push(wl_output);
                                }
                            }
                        }
                    }
                    instance.done();
                }
            }

            for outputs in data.instances.values_mut() {
                // Clean up dead wl_outputs.
                outputs.retain(|x| x.is_alive());
            }
        }
        Entry::Vacant(entry) => {
            // New window, start tracking it.
            let mut data = ToplevelData {
                title: role.title.clone(),
                app_id: role.app_id.clone(),
                states,
                output: output.cloned(),
                instances: HashMap::new(),
            };

            for manager in &protocol_state.instances {
                if let Some(client) = manager.client() {
                    data.add_instance::<AnvilState<D>>(&protocol_state.display, &client, manager);
                }
            }

            entry.insert(data);
        }
    }
    has_focus
}

impl ToplevelData {
    fn add_instance<D>(
        &mut self,
        handle: &DisplayHandle,
        client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
    ) where
        D: Dispatch<ZwlrForeignToplevelHandleV1, ()>,
        D: 'static,
    {
        let toplevel = client
            .create_resource::<ZwlrForeignToplevelHandleV1, _, D>(handle, manager.version(), ())
            .unwrap();
        manager.toplevel(&toplevel);

        if let Some(title) = &self.title {
            toplevel.title(title.clone());
        }
        if let Some(app_id) = &self.app_id {
            toplevel.app_id(app_id.clone());
        }

        toplevel.state(self.states.iter().flat_map(|x| x.to_ne_bytes()).collect());

        let mut outputs = Vec::new();
        if let Some(output) = &self.output {
            for wl_output in output.client_outputs(client) {
                toplevel.output_enter(&wl_output);
                outputs.push(wl_output);
            }
        }

        toplevel.done();

        self.instances.insert(toplevel, outputs);
    }
}

impl<D> GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData, D>
    for ForeignToplevelManagerState
where
    D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData>,
    D: Dispatch<ZwlrForeignToplevelManagerV1, ()>,
    D: Dispatch<ZwlrForeignToplevelHandleV1, ()>,
    D: ForeignToplevelHandler,
{
    fn bind(
        state: &mut D,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &ForeignToplevelGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        let state = state.foreign_toplevel_manager_state();

        for data in state.toplevels.values_mut() {
            data.add_instance::<D>(handle, client, &manager);
        }

        state.instances.push(manager);
    }

    fn can_view(client: Client, global_data: &ForeignToplevelGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrForeignToplevelManagerV1, (), D> for ForeignToplevelManagerState
where
    D: Dispatch<ZwlrForeignToplevelManagerV1, ()>,
    D: ForeignToplevelHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: <ZwlrForeignToplevelManagerV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_foreign_toplevel_manager_v1::Request::Stop => {
                resource.finished();

                let state = state.foreign_toplevel_manager_state();
                state.instances.retain(|x| x != resource);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        let state = state.foreign_toplevel_manager_state();
        state.instances.retain(|x| x != resource);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelHandleV1, (), D> for ForeignToplevelManagerState
where
    D: Dispatch<ZwlrForeignToplevelHandleV1, ()>,
    D: ForeignToplevelHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: <ZwlrForeignToplevelHandleV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let protocol_state = state.foreign_toplevel_manager_state();

        let Some((surface, _)) = protocol_state
            .toplevels
            .iter()
            .find(|(_, data)| data.instances.contains_key(resource))
        else {
            return;
        };
        let surface = surface.clone();

        match request {
            zwlr_foreign_toplevel_handle_v1::Request::SetMaximized => {
                state.set_maximized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMaximized => {
                state.unset_maximized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetMinimized => {
                state.set_minimized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetMinimized => {
                state.unset_minimized(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Activate { .. } => {
                state.activate(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Close => {
                state.close(surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::SetRectangle { .. } => (),
            zwlr_foreign_toplevel_handle_v1::Request::Destroy => (),
            zwlr_foreign_toplevel_handle_v1::Request::SetFullscreen { output } => {
                state.set_fullscreen(surface, output);
            }
            zwlr_foreign_toplevel_handle_v1::Request::UnsetFullscreen => {
                state.unset_fullscreen(surface);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        _data: &(),
    ) {
        let state = state.foreign_toplevel_manager_state();
        for data in state.toplevels.values_mut() {
            data.instances.retain(|instance, _| instance != resource);
        }
    }
}

fn to_state_vec(states: &ToplevelStateSet, has_focus: bool) -> ArrayVec<u32, 3> {
    let mut rv = ArrayVec::new();
    if states.contains(xdg_toplevel::State::Maximized) {
        rv.push(zwlr_foreign_toplevel_handle_v1::State::Maximized as u32);
    }
    if states.contains(xdg_toplevel::State::Fullscreen) {
        rv.push(zwlr_foreign_toplevel_handle_v1::State::Fullscreen as u32);
    }
    if has_focus {
        rv.push(zwlr_foreign_toplevel_handle_v1::State::Activated as u32);
    }

    rv
}

#[macro_export]
macro_rules! delegate_foreign_toplevel {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: $crate::shell::toplevel_manager::ForeignToplevelGlobalData
        ] => $crate::shell::toplevel_manager::ForeignToplevelManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: ()
        ] => $crate::shell::toplevel_manager::ForeignToplevelManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1: ()
        ] => $crate::shell::toplevel_manager::ForeignToplevelManagerState);
    };
}
