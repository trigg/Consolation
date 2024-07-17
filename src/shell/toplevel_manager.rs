use std::sync::{Arc, Mutex};

use smithay::{
    desktop::Window,
    input::SeatHandler,
    reexports::{
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::*, zwlr_foreign_toplevel_manager_v1::*,
        },
        wayland_server::{
            backend::GlobalId, protocol::wl_output::WlOutput, Client, Dispatch, DisplayHandle,
            GlobalDispatch,
        },
    },
};

use crate::state::update_toplevel_handle;

#[derive(Debug)]
pub struct TopLevelManager {
    global: GlobalId,
    pub(crate) managers: Vec<TopLevelMan>,
}

#[derive(Debug)]
pub struct TopLevelMan {
    pub(crate) client: Client,
    pub(crate) handler: DisplayHandle,
    pub(crate) resource: ZwlrForeignToplevelManagerV1,
}

pub struct TopLevelManagerHandle {
    pub handlers: Arc<Mutex<Vec<ZwlrForeignToplevelHandleV1>>>,
}

#[derive(Clone)]
pub struct TopLevelHandle {}

impl TopLevelManager {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ()>,
        D: Dispatch<ZwlrForeignToplevelManagerV1, ()>,
        D: Dispatch<ZwlrForeignToplevelHandleV1, ()>,
        D: SeatHandler,
        D: 'static,
    {
        let global = display.create_global::<D, ZwlrForeignToplevelManagerV1, ()>(3, ());
        Self {
            global,
            managers: Default::default(),
        }
    }

    pub fn new_toplevel<D>(&self, window: &Window) -> Vec<ZwlrForeignToplevelHandleV1>
    where
        D: Dispatch<ZwlrForeignToplevelHandleV1, ()>,
        D: 'static,
    {
        self.managers
            .iter()
            .map(|manager| {
                let a = manager
                    .client
                    .create_resource::<ZwlrForeignToplevelHandleV1, (), D>(&manager.handler, 3, ())
                    .unwrap();
                manager.resource.toplevel(&a);
                update_toplevel_handle(window.clone());
                return a;
            })
            .collect()
    }

    pub fn get_window(
        &self,
        windows: &Vec<Window>,
        toplevel: &ZwlrForeignToplevelHandleV1,
    ) -> Option<Window> {
        for window in windows {
            if window
                .user_data()
                .get::<ZwlrForeignToplevelHandleV1>()
                .unwrap()
                == toplevel
            {
                return Some(window.clone());
            }
        }
        return None;
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

pub fn set_title(list: &Vec<ZwlrForeignToplevelHandleV1>, title: String) {
    list.iter().for_each(|handle| {
        handle.title(title.clone());
    })
}

pub fn set_appid(list: &Vec<ZwlrForeignToplevelHandleV1>, appid: String) {
    list.iter().for_each(|handle| {
        handle.app_id(appid.clone());
    })
}

pub fn output_enter(list: &Vec<ZwlrForeignToplevelHandleV1>, output: WlOutput) {
    list.iter().for_each(|handle| {
        handle.output_enter(&output);
    })
}

pub fn output_leave(list: &Vec<ZwlrForeignToplevelHandleV1>, output: WlOutput) {
    list.iter().for_each(|handle| {
        handle.output_leave(&output);
    })
}

pub fn set_state(list: &Vec<ZwlrForeignToplevelHandleV1>, state: Vec<u8>) {
    list.iter().for_each(|handle| {
        handle.state(state.clone());
    })
}

pub fn closed(list: &Vec<ZwlrForeignToplevelHandleV1>) {
    list.iter().for_each(|handle| {
        handle.closed();
    })
}

pub fn set_parent(
    list: &Vec<ZwlrForeignToplevelHandleV1>,
    parent: Option<&ZwlrForeignToplevelHandleV1>,
) {
    list.iter().for_each(|handle| {
        handle.parent(parent);
    })
}

pub fn done(list: &Vec<ZwlrForeignToplevelHandleV1>) {
    tracing::info!("Sending DONE to {} clients", list.len());
    list.iter().for_each(|handle| {
        handle.done();
    })
}
