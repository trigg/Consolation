use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::iter::zip;
use std::mem;

use smithay::reexports::drm::control::{self, ModeTypeFlags};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1, zwlr_output_configuration_v1, zwlr_output_head_v1,
    zwlr_output_manager_v1, zwlr_output_mode_v1,
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::Transform;
use zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1;
use zwlr_output_configuration_v1::ZwlrOutputConfigurationV1;
use zwlr_output_head_v1::{AdaptiveSyncState, ZwlrOutputHeadV1};
use zwlr_output_manager_v1::ZwlrOutputManagerV1;
use zwlr_output_mode_v1::ZwlrOutputModeV1;

use crate::udev::UdevData;
use crate::AnvilState;

const VERSION: u32 = 4;

#[derive(Debug, Default, Clone)]
pub struct Output {
    pub(crate) mode: Option<Mode>,
    pub(crate) modes: Vec<Mode>,
    pub(crate) current_mode: Option<usize>,
    pub(crate) vrr_enabled: bool,
    pub(crate) name: String,
    pub(crate) logical: Option<LogicalOutput>,
    pub(crate) transform: Transform,
    pub(crate) scale: Option<f64>,
    pub(crate) off: bool,
    pub(crate) variable_refresh_rate: bool,
    pub(crate) make: String,
    pub(crate) model: String,
    pub(crate) physical_size: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Default, Copy, PartialEq)]
pub struct LogicalOutput {
    /// Logical X position.
    pub x: i32,
    /// Logical Y position.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
    /// Scale factor.
    pub scale: f64,
    /// Transform.
    pub transform: Transform,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    pub width: u16,
    pub height: u16,
    pub refresh_rate: u32,
    pub is_preferred: bool,
}

impl From<&control::Mode> for Mode {
    fn from(value: &control::Mode) -> Self {
        let mode = smithay::output::Mode::from(*value);
        Mode {
            width: value.size().0,
            height: value.size().1,
            refresh_rate: mode.refresh as u32,
            is_preferred: value.mode_type().contains(ModeTypeFlags::PREFERRED),
        }
    }
}

#[derive(Debug, Hash, Clone, Copy, PartialEq, Eq)]
pub struct OutputId(pub u32);

#[derive(Debug, Default, Clone)]
pub struct Outputs(pub Vec<Output>);

impl FromIterator<Output> for Outputs {
    fn from_iter<T: IntoIterator<Item = Output>>(iter: T) -> Self {
        Self(Vec::from_iter(iter))
    }
}

impl Outputs {
    pub fn find(&self, name: &str) -> Option<&Output> {
        self.0.iter().find(|o| o.name.eq_ignore_ascii_case(name))
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut Output> {
        self.0
            .iter_mut()
            .find(|o| o.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug)]
struct ClientData {
    heads: HashMap<OutputId, (ZwlrOutputHeadV1, Vec<ZwlrOutputModeV1>)>,
    confs: HashMap<ZwlrOutputConfigurationV1, OutputConfigurationState>,
    manager: ZwlrOutputManagerV1,
}

#[derive(Debug)]
pub struct OutputManagementManagerState {
    display: DisplayHandle,
    serial: u32,
    clients: HashMap<ClientId, ClientData>,
    current_state: HashMap<OutputId, Output>,
    current_config: Outputs,
}

pub struct OutputManagementManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

pub trait OutputManagementHandler {
    fn output_management_state(&mut self) -> &mut OutputManagementManagerState;
    fn apply_output_config(&mut self, config: Outputs);
}

#[derive(Debug)]
enum OutputConfigurationState {
    Ongoing(HashMap<OutputId, Output>),
    Finished,
}

pub enum OutputConfigurationHeadState {
    Cancelled,
    Ok(OutputId, ZwlrOutputConfigurationV1),
}

impl OutputManagementManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
        D: Dispatch<ZwlrOutputManagerV1, ()>,
        D: Dispatch<ZwlrOutputHeadV1, OutputId>,
        D: Dispatch<ZwlrOutputConfigurationV1, u32>,
        D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
        D: Dispatch<ZwlrOutputModeV1, ()>,
        D: OutputManagementHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = OutputManagementManagerGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, ZwlrOutputManagerV1, _>(VERSION, global_data);

        Self {
            display: display.clone(),
            clients: HashMap::new(),
            serial: 0,
            current_state: HashMap::new(),
            current_config: Default::default(),
        }
    }

    pub fn on_config_changed(&mut self, new_config: Outputs) {
        self.current_config = new_config;
    }

    pub fn notify_changes(&mut self, new_state: HashMap<OutputId, Output>) {
        let mut changed = false; /* most likely to end up true */
        for (output, conf) in new_state.iter() {
            if let Some(old) = self.current_state.get(output) {
                if old.vrr_enabled != conf.vrr_enabled {
                    changed = true;
                    for client in self.clients.values() {
                        if let Some((head, _)) = client.heads.get(output) {
                            if head.version() >= zwlr_output_head_v1::EVT_ADAPTIVE_SYNC_SINCE {
                                head.adaptive_sync(match conf.vrr_enabled {
                                    true => AdaptiveSyncState::Enabled,
                                    false => AdaptiveSyncState::Disabled,
                                });
                            }
                        }
                    }
                }

                let modes_changed = old.modes != conf.modes;
                if modes_changed {
                    changed = true;
                    if old.modes.len() != conf.modes.len() {
                        println!("output's old mode count doesn't match new modes");
                    } else {
                        for client in self.clients.values() {
                            if let Some((_, modes)) = client.heads.get(output) {
                                for (wl_mode, mode) in zip(modes, &conf.modes) {
                                    wl_mode.size(i32::from(mode.width), i32::from(mode.height));
                                    if let Ok(refresh_rate) = mode.refresh_rate.try_into() {
                                        wl_mode.refresh(refresh_rate);
                                    }
                                }
                            }
                        }
                    }
                }

                match (old.current_mode, conf.current_mode) {
                    (Some(old_index), Some(new_index)) => {
                        if old.modes.len() == conf.modes.len()
                            && (modes_changed || old_index != new_index)
                        {
                            changed = true;
                            for client in self.clients.values() {
                                if let Some((head, modes)) = client.heads.get(output) {
                                    if let Some(new_mode) = modes.get(new_index) {
                                        head.current_mode(new_mode);
                                    } else {
                                        println!(
                                            "output new mode doesnt exist for the client's output"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    (Some(_), None) => {
                        changed = true;
                        for client in self.clients.values() {
                            if let Some((head, _)) = client.heads.get(output) {
                                head.enabled(0);
                            }
                        }
                    }
                    (None, Some(new_index)) => {
                        if old.modes.len() == conf.modes.len() {
                            changed = true;
                            for client in self.clients.values() {
                                if let Some((head, modes)) = client.heads.get(output) {
                                    head.enabled(1);
                                    if let Some(mode) = modes.get(new_index) {
                                        head.current_mode(mode);
                                    } else {
                                        println!(
                                            "output new mode doesnt exist for the client's output"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    (None, None) => {}
                }
                match (old.logical, conf.logical) {
                    (Some(old_logical), Some(new_logical)) => {
                        if old_logical != new_logical {
                            changed = true;
                            for client in self.clients.values() {
                                if let Some((head, _)) = client.heads.get(output) {
                                    if old_logical.x != new_logical.x
                                        || old_logical.y != new_logical.y
                                    {
                                        head.position(new_logical.x, new_logical.y);
                                    }
                                    if old_logical.scale != new_logical.scale {
                                        head.scale(new_logical.scale);
                                    }
                                    if old_logical.transform != new_logical.transform {
                                        head.transform(new_logical.transform.into());
                                    }
                                }
                            }
                        }
                    }
                    (None, Some(new_logical)) => {
                        changed = true;
                        for client in self.clients.values() {
                            if let Some((head, _)) = client.heads.get(output) {
                                // head enable in the mode diff check
                                head.position(new_logical.x, new_logical.y);
                                head.transform(new_logical.transform.into());
                                head.scale(new_logical.scale);
                            }
                        }
                    }
                    (Some(_), None) => {
                        // heads disabled in the mode diff check
                    }
                    (None, None) => {}
                }
            } else {
                changed = true;
                notify_new_head(self, output, conf);
            }
        }
        for (old, _) in self.current_state.iter() {
            if !new_state.contains_key(old) {
                changed = true;
                notify_removed_head(&mut self.clients, old);
            }
        }
        if changed {
            self.current_state = new_state;
            self.serial += 1;
            for data in self.clients.values() {
                data.manager.done(self.serial);
                for conf in data.confs.keys() {
                    conf.cancelled();
                }
            }
        }
    }
}

impl<D> GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData, D>
    for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn bind(
        state: &mut D,
        display: &DisplayHandle,
        client: &Client,
        manager: New<ZwlrOutputManagerV1>,
        _manager_state: &OutputManagementManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(manager, ());
        let g_state = state.output_management_state();
        let mut client_data = ClientData {
            heads: HashMap::new(),
            confs: HashMap::new(),
            manager: manager.clone(),
        };
        for (output, conf) in &g_state.current_state {
            send_new_head::<D>(display, client, &mut client_data, *output, conf);
        }
        g_state.clients.insert(client.id(), client_data);
        manager.done(g_state.serial);
    }

    fn can_view(client: Client, global_data: &OutputManagementManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrOutputManagerV1, (), D> for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        client: &Client,
        _manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                let g_state = state.output_management_state();
                let conf = data_init.init(id, serial);
                if let Some(client_data) = g_state.clients.get_mut(&client.id()) {
                    if serial != g_state.serial {
                        conf.cancelled();
                    }
                    let state = OutputConfigurationState::Ongoing(HashMap::new());
                    client_data.confs.insert(conf, state);
                } else {
                    println!("CreateConfiguration: missing client data");
                }
            }
            zwlr_output_manager_v1::Request::Stop => {
                if let Some(c) = state.output_management_state().clients.remove(&client.id()) {
                    c.manager.finished()
                }
            }
            _ => unreachable!(),
        }
    }
    fn destroyed(state: &mut D, client: ClientId, _resource: &ZwlrOutputManagerV1, _data: &()) {
        state.output_management_state().clients.remove(&client);
    }
}

impl<D> Dispatch<ZwlrOutputConfigurationV1, u32, D> for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        client: &Client,
        conf: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        serial: &u32,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let g_state = state.output_management_state();
        let outdated = *serial != g_state.serial;
        if outdated {
            println!("OutputConfiguration: request from an outdated configuration");
        }

        let new_config = g_state
            .clients
            .get_mut(&client.id())
            .and_then(|data| data.confs.get_mut(conf));
        if new_config.is_none() {
            println!("OutputConfiguration: request from unknown configuration object");
        }

        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, head } => {
                let Some(output) = head.data::<OutputId>() else {
                    println!("EnableHead: Missing attached output");
                    let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                    return;
                };
                if outdated {
                    let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                    return;
                }

                let Some(new_config) = new_config else {
                    let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                    return;
                };

                let OutputConfigurationState::Ongoing(new_config) = new_config else {
                    let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                    conf.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration had already been used",
                    );
                    return;
                };

                let Some(current_config) = g_state.current_state.get(output) else {
                    println!("EnableHead: output missing from current config");
                    let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                    return;
                };

                match new_config.entry(*output) {
                    Entry::Occupied(_) => {
                        let _fail = data_init.init(id, OutputConfigurationHeadState::Cancelled);
                        conf.post_error(
                            zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                            "head has been already configured",
                        );
                        return;
                    }
                    Entry::Vacant(entry) => {
                        let mut config = g_state
                            .current_config
                            .find(&current_config.name)
                            .cloned()
                            .unwrap_or_else(|| Output {
                                name: current_config.name.clone(),
                                ..Default::default()
                            });
                        config.off = false;
                        entry.insert(config);
                    }
                };

                data_init.init(id, OutputConfigurationHeadState::Ok(*output, conf.clone()));
            }
            zwlr_output_configuration_v1::Request::DisableHead { head } => {
                if outdated {
                    return;
                }
                let Some(output) = head.data::<OutputId>() else {
                    println!("DisableHead: missing attached output head name");
                    return;
                };

                let Some(new_config) = new_config else {
                    return;
                };

                let OutputConfigurationState::Ongoing(new_config) = new_config else {
                    conf.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration had already been used",
                    );
                    return;
                };

                let Some(current_config) = g_state.current_state.get(output) else {
                    println!("EnableHead: output missing from current config");
                    return;
                };

                match new_config.entry(*output) {
                    Entry::Occupied(_) => {
                        conf.post_error(
                            zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                            "head has been already configured",
                        );
                    }
                    Entry::Vacant(entry) => {
                        let mut config = g_state
                            .current_config
                            .find(&current_config.name)
                            .cloned()
                            .unwrap_or_else(|| Output {
                                name: current_config.name.clone(),
                                ..Default::default()
                            });
                        config.off = true;
                        entry.insert(config);
                    }
                };
            }
            zwlr_output_configuration_v1::Request::Apply => {
                if outdated {
                    conf.cancelled();
                    return;
                }

                let Some(new_config) = new_config else {
                    return;
                };

                let OutputConfigurationState::Ongoing(new_config) =
                    mem::replace(new_config, OutputConfigurationState::Finished)
                else {
                    conf.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration had already been used",
                    );
                    return;
                };

                let any_enabled = new_config.values().any(|c| !c.off);
                if !any_enabled {
                    conf.failed();
                    return;
                }

                state.apply_output_config(new_config.into_values().collect());
                conf.succeeded();
            }
            zwlr_output_configuration_v1::Request::Test => {
                if outdated {
                    conf.cancelled();
                    return;
                }

                let Some(new_config) = new_config else {
                    return;
                };

                let OutputConfigurationState::Ongoing(new_config) =
                    mem::replace(new_config, OutputConfigurationState::Finished)
                else {
                    conf.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "configuration had already been used",
                    );
                    return;
                };

                let any_enabled = new_config.values().any(|c| !c.off);
                if !any_enabled {
                    conf.failed();
                    return;
                }

                // FIXME: actually test the configuration with TTY.
                conf.succeeded()
            }
            zwlr_output_configuration_v1::Request::Destroy => {
                g_state
                    .clients
                    .get_mut(&client.id())
                    .map(|d| d.confs.remove(conf));
            }
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState, D>
    for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        client: &Client,
        conf_head: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &OutputConfigurationHeadState,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let g_state = state.output_management_state();
        let Some(client_data) = g_state.clients.get_mut(&client.id()) else {
            println!("ConfigurationHead: missing client data");
            return;
        };
        let OutputConfigurationHeadState::Ok(output_id, conf) = data else {
            println!("ConfigurationHead: request sent to a cancelled head");
            return;
        };
        let Some(serial) = conf.data::<u32>() else {
            println!("ConfigurationHead: missing serial");
            return;
        };
        if *serial != g_state.serial {
            println!("ConfigurationHead: request sent to an outdated");
            return;
        }
        let Some(new_config) = client_data.confs.get_mut(conf) else {
            println!("ConfigurationHead: unknown configuration");
            return;
        };
        let OutputConfigurationState::Ongoing(new_config) = new_config else {
            conf.post_error(
                zwlr_output_configuration_v1::Error::AlreadyUsed,
                "configuration had already been used",
            );
            return;
        };
        let Some(new_config) = new_config.get_mut(output_id) else {
            println!("ConfigurationHead: config missing from enabled heads");
            return;
        };

        match request {
            zwlr_output_configuration_head_v1::Request::SetMode { mode } => {
                let index = match client_data
                    .heads
                    .get(output_id)
                    .map(|(_, mods)| mods.iter().position(|m| m.id() == mode.id()))
                {
                    Some(Some(index)) => index,
                    _ => {
                        println!("SetMode: failed to find requested mode");
                        conf_head.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidMode,
                            "failed to find requested mode",
                        );
                        return;
                    }
                };

                let Some(current_config) = g_state.current_state.get(output_id) else {
                    println!("SetMode: output missing from the current config");
                    return;
                };

                let Some(mode) = current_config.modes.get(index) else {
                    println!("SetMode: requested mode is out of range");
                    return;
                };

                new_config.mode = Some(mode.clone());
            }
            zwlr_output_configuration_head_v1::Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                // FIXME: Support custom mode
                let (width, height, refresh): (u16, u16, u32) =
                    match (width.try_into(), height.try_into(), refresh.try_into()) {
                        (Ok(width), Ok(height), Ok(refresh)) => (width, height, refresh),
                        _ => {
                            println!("SetCustomMode: invalid input data");
                            return;
                        }
                    };

                let Some(current_config) = g_state.current_state.get(output_id) else {
                    println!("SetMode: output missing from the current config");
                    return;
                };

                let Some(mode) = current_config.modes.iter().find(|m| {
                    m.width == width
                        && m.height == height
                        && (refresh == 0 || m.refresh_rate == refresh)
                }) else {
                    println!("SetCustomMode: no matching mode");
                    return;
                };

                new_config.mode = Some(mode.clone());
            }
            zwlr_output_configuration_head_v1::Request::SetPosition { x: _, y: _ } => {
                // Do nothing
            }
            zwlr_output_configuration_head_v1::Request::SetTransform { transform } => {
                new_config.transform = match transform {
                    WEnum::Value(wl_output::Transform::Normal) => Transform::Normal,
                    WEnum::Value(wl_output::Transform::_90) => Transform::_90,
                    WEnum::Value(wl_output::Transform::_180) => Transform::_180,
                    WEnum::Value(wl_output::Transform::_270) => Transform::_270,
                    WEnum::Value(wl_output::Transform::Flipped) => Transform::Flipped,
                    WEnum::Value(wl_output::Transform::Flipped90) => Transform::Flipped90,
                    WEnum::Value(wl_output::Transform::Flipped180) => Transform::Flipped180,
                    WEnum::Value(wl_output::Transform::Flipped270) => Transform::Flipped270,
                    WEnum::Value(_) => Transform::Normal,
                    WEnum::Unknown(_) => Transform::Normal,
                };
            }
            zwlr_output_configuration_head_v1::Request::SetScale { scale } => {
                if scale <= 0. {
                    conf_head.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidScale,
                        "scale is negative or zero",
                    );
                    return;
                }
                new_config.scale = Some(scale);
            }
            zwlr_output_configuration_head_v1::Request::SetAdaptiveSync { state } => {
                let enabled = match state {
                    WEnum::Value(AdaptiveSyncState::Enabled) => true,
                    WEnum::Value(AdaptiveSyncState::Disabled) => false,
                    _ => {
                        println!("SetAdaptativeSync: unknown requested adaptative sync");
                        conf_head.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidAdaptiveSyncState,
                            "unknown adaptive sync value",
                        );
                        return;
                    }
                };
                new_config.variable_refresh_rate = enabled;
            }
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ZwlrOutputHeadV1, OutputId, D> for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _output_head: &ZwlrOutputHeadV1,
        request: zwlr_output_head_v1::Request,
        _data: &OutputId,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_output_head_v1::Request::Release => {}
            _ => unreachable!(),
        }
    }
    fn destroyed(state: &mut D, client: ClientId, _resource: &ZwlrOutputHeadV1, data: &OutputId) {
        if let Some(c) = state.output_management_state().clients.get_mut(&client) {
            c.heads.remove(data);
        }
    }
}

impl<D> Dispatch<ZwlrOutputModeV1, (), D> for OutputManagementManagerState
where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        request: zwlr_output_mode_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_output_mode_v1::Request::Release => {}
            _ => unreachable!(),
        }
    }
}

#[macro_export]
macro_rules! delegate_output_management{
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_manager_v1::ZwlrOutputManagerV1: $crate::shell::output_manager::OutputManagementManagerGlobalData
        ] => $crate::shell::output_manager::OutputManagementManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_manager_v1::ZwlrOutputManagerV1: ()
        ] => $crate::shell::output_manager::OutputManagementManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_v1::ZwlrOutputConfigurationV1: u32
        ] => $crate::shell::output_manager::OutputManagementManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_head_v1::ZwlrOutputHeadV1: $crate::shell::output_manager::OutputId
        ] => $crate::shell::output_manager::OutputManagementManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_mode_v1::ZwlrOutputModeV1: ()
        ] => $crate::shell::output_manager::OutputManagementManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1: $crate::shell::output_manager::OutputConfigurationHeadState
        ] => $crate::shell::output_manager::OutputManagementManagerState);
    };
}

fn notify_removed_head(clients: &mut HashMap<ClientId, ClientData>, head: &OutputId) {
    for data in clients.values_mut() {
        if let Some((head, mods)) = data.heads.remove(head) {
            mods.iter().for_each(|m| m.finished());
            head.finished();
        }
    }
}

fn notify_new_head(state: &mut OutputManagementManagerState, output: &OutputId, conf: &Output) {
    let display = &state.display;
    let clients = &mut state.clients;
    for data in clients.values_mut() {
        if let Some(client) = data.manager.client() {
            send_new_head::<AnvilState<UdevData>>(display, &client, data, *output, conf);
        }
    }
}

fn send_new_head<D>(
    display: &DisplayHandle,
    client: &Client,
    client_data: &mut ClientData,
    output: OutputId,
    conf: &Output,
) where
    D: GlobalDispatch<ZwlrOutputManagerV1, OutputManagementManagerGlobalData>,
    D: Dispatch<ZwlrOutputManagerV1, ()>,
    D: Dispatch<ZwlrOutputConfigurationV1, u32>,
    D: Dispatch<ZwlrOutputConfigurationHeadV1, OutputConfigurationHeadState>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: OutputManagementHandler,
    D: 'static,
    D: Dispatch<ZwlrOutputHeadV1, OutputId>,
    D: Dispatch<ZwlrOutputModeV1, ()>,
    D: 'static,
{
    let new_head = client
        .create_resource::<ZwlrOutputHeadV1, _, D>(display, client_data.manager.version(), output)
        .unwrap();
    client_data.manager.head(&new_head);
    new_head.name(conf.name.clone());
    new_head.description(format!("{} - {} - {}", conf.make, conf.model, conf.name));
    if let Some((width, height)) = conf.physical_size {
        if let (Ok(a), Ok(b)) = (width.try_into(), height.try_into()) {
            new_head.physical_size(a, b);
        }
    }
    let mut new_modes = Vec::with_capacity(conf.modes.len());
    for (index, mode) in conf.modes.iter().enumerate() {
        let new_mode = client
            .create_resource::<ZwlrOutputModeV1, _, D>(display, new_head.version(), ())
            .unwrap();
        new_head.mode(&new_mode);
        new_mode.size(i32::from(mode.width), i32::from(mode.height));
        if mode.is_preferred {
            new_mode.preferred();
        }
        if let Ok(refresh_rate) = mode.refresh_rate.try_into() {
            new_mode.refresh(refresh_rate);
        }
        if Some(index) == conf.current_mode {
            new_head.current_mode(&new_mode);
        }
        new_modes.push(new_mode);
    }
    if let Some(logical) = conf.logical {
        new_head.position(logical.x, logical.y);
        new_head.transform(logical.transform.into());
        new_head.scale(logical.scale);
    }
    new_head.enabled(conf.current_mode.is_some() as i32);
    if new_head.version() >= zwlr_output_head_v1::EVT_MAKE_SINCE {
        new_head.make(conf.make.clone());
    }
    if new_head.version() >= zwlr_output_head_v1::EVT_MODEL_SINCE {
        new_head.model(conf.model.clone());
    }

    if new_head.version() >= zwlr_output_head_v1::EVT_ADAPTIVE_SYNC_SINCE {
        new_head.adaptive_sync(match conf.vrr_enabled {
            true => AdaptiveSyncState::Enabled,
            false => AdaptiveSyncState::Disabled,
        });
    }
    // new_head.serial_number(output.serial);
    client_data.heads.insert(output, (new_head, new_modes));
}
