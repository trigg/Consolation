use std::{
    cell::RefCell, collections::HashMap, convert::TryFrom, os::unix::net::UnixStream, rc::Rc,
    sync::Arc, sync::Mutex,
};

use smithay::{
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Client},
    utils::{x11rb::X11Source, Logical, Point},
    wayland::compositor::{give_role, with_states},
};

use x11rb::{
    connection::Connection as _,
    errors::ReplyOrIdError,
    protocol::{
        composite::{ConnectionExt as _, Redirect},
        xproto::{
            ChangeWindowAttributesAux, ConfigWindow, ConfigureWindowAux, ConnectionExt as _,
            EventMask, Window, WindowClass,
        },
        Event,
    },
    rust_connection::{DefaultStream, RustConnection},
};

use crate::{
    output_map::OutputMap,
    window_map::{Kind, TitleContainer, WindowMap, X11Id},
    ConsolationState,
};

impl<BackendData: 'static> ConsolationState<BackendData> {
    pub fn start_xwayland(&mut self) {
        if let Err(e) = self.xwayland.start() {
            error!(self.log, "Failed to start XWayland: {}", e);
        }
    }

    pub fn xwayland_ready(&mut self, connection: UnixStream, client: Client) {
        let (wm, source) = X11State::start_wm(
            connection,
            self.window_map.clone(),
            self.output_map.clone(),
            self.log.clone(),
        )
        .unwrap();
        let wm = Rc::new(RefCell::new(wm));
        client.data_map().insert_if_missing(|| Rc::clone(&wm));
        let log = self.log.clone();
        self.handle
            .insert_source(source, move |event, _, _| {
                match wm.borrow_mut().handle_event(event, &client) {
                    Ok(()) => {}
                    Err(err) => error!(log, "Error while handling X11 event: {}", err),
                }
            })
            .unwrap();
    }

    pub fn xwayland_exited(&mut self) {
        error!(self.log, "Xwayland crashed");
    }
}

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        ATOM,
        WM_S0,
        WL_SURFACE_ID,
        _CONSOLATION_CLOSE_CONNECTION,
        // Window title atoms
        XA_WM_NAME,
        WM_NAME,
        _NET_WM_NAME,
        // Types of string
        UTF8_STRING,
        STRING,
        // Popup menu detection
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_MENU,
    }
}

/// The actual runtime state of the XWayland integration.
struct X11State {
    conn: Arc<RustConnection>,
    atoms: Atoms,
    log: slog::Logger,
    unpaired_surfaces: HashMap<u32, (Window, Point<i32, Logical>)>,
    window_map: Rc<RefCell<WindowMap>>,
    output_map: Rc<RefCell<OutputMap>>,
}

impl X11State {
    fn start_wm(
        connection: UnixStream,
        window_map: Rc<RefCell<WindowMap>>,
        output_map: Rc<RefCell<OutputMap>>,
        log: slog::Logger,
    ) -> Result<(Self, X11Source), Box<dyn std::error::Error>> {
        // Create an X11 connection. XWayland only uses screen 0.
        let screen = 0;
        let stream = DefaultStream::from_unix_stream(connection)?;
        let conn = RustConnection::connect_to_stream(stream, screen)?;
        let atoms = Atoms::new(&conn)?.reply()?;

        let screen = &conn.setup().roots[0];

        // Actually become the WM by redirecting some operations
        conn.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
        )?;

        // Tell XWayland that we are the WM by acquiring the WM_S0 selection. No X11 clients are accepted before this.
        let win = conn.generate_id()?;
        conn.create_window(
            screen.root_depth,
            win,
            screen.root,
            // x, y, width, height, border width
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &Default::default(),
        )?;
        conn.set_selection_owner(win, atoms.WM_S0, x11rb::CURRENT_TIME)?;

        // XWayland wants us to do this to function properly...?
        conn.composite_redirect_subwindows(screen.root, Redirect::MANUAL)?;

        conn.flush()?;

        let conn = Arc::new(conn);
        let wm = Self {
            conn: Arc::clone(&conn),
            atoms,
            unpaired_surfaces: Default::default(),
            window_map,
            log: log.clone(),
            output_map,
        };

        Ok((
            wm,
            X11Source::new(conn, win, atoms._CONSOLATION_CLOSE_CONNECTION, log),
        ))
    }

    fn handle_event(&mut self, event: Event, client: &Client) -> Result<(), ReplyOrIdError> {
        debug!(self.log, "X11: Got event {:?}", event);
        match event {
            Event::ConfigureRequest(r) => {
                // Just grant the wish
                let mut aux = ConfigureWindowAux::default();
                if r.value_mask & u16::from(ConfigWindow::STACK_MODE) != 0 {
                    aux = aux.stack_mode(r.stack_mode);
                }
                if r.value_mask & u16::from(ConfigWindow::SIBLING) != 0 {
                    aux = aux.sibling(r.sibling);
                }
                if r.value_mask & u16::from(ConfigWindow::X) != 0 {
                    //aux = aux.x(i32::try_from(r.x).unwrap());
                    aux = aux.x(0);
                }
                if r.value_mask & u16::from(ConfigWindow::Y) != 0 {
                    //aux = aux.y(i32::try_from(r.y).unwrap());
                    aux = aux.y(0);
                }
                //if r.value_mask & u16::from(ConfigWindow::WIDTH) != 0 {
                //aux = aux.width(u32::try_from(r.width).unwrap());
                aux = aux.width(
                    self.output_map
                        .borrow_mut()
                        .find_by_index(0)
                        .unwrap()
                        .size()
                        .w as u32,
                );
                //}
                //if r.value_mask & u16::from(ConfigWindow::HEIGHT) != 0 {
                //aux = aux.height(u32::try_from(r.height).unwrap());
                aux = aux.height(
                    self.output_map
                        .borrow_mut()
                        .find_by_index(0)
                        .unwrap()
                        .size()
                        .h as u32,
                );
                //}
                if r.value_mask & u16::from(ConfigWindow::BORDER_WIDTH) != 0 {
                    aux = aux.border_width(u32::try_from(r.border_width).unwrap());
                }
                self.conn.configure_window(r.window, &aux)?;
            }
            Event::MapRequest(r) => {
                // Just grant the wish
                self.conn.map_window(r.window)?;
                self.update_title_x11(r.window);
            }
            Event::ClientMessage(msg) => {
                if msg.type_ == self.atoms.WL_SURFACE_ID {
                    // We get a WL_SURFACE_ID message when Xwayland creates a WlSurface for a
                    // window. Both the creation of the surface and this client message happen at
                    // roughly the same time and are sent over different sockets (X11 socket and
                    // wayland socket). Thus, we could receive these two in any order. Hence, it
                    // can happen that we get None below when X11 was faster than Wayland.
                    let location = {
                        match self.conn.get_geometry(msg.window)?.reply() {
                            Ok(geo) => (geo.x as i32, geo.y as i32).into(),
                            Err(err) => {
                                error!(
                                    self.log,
                                    "Failed to get geometry for {:x}, perhaps the window was already destroyed?",
                                    msg.window;
                                    "err" => format!("{:?}", err),
                                );
                                (0, 0).into()
                            }
                        }
                    };

                    let id = msg.data.as_data32()[0];
                    let surface = client.get_resource::<WlSurface>(id);
                    info!(
                        self.log,
                        "X11 surface {:x?} corresponds to WlSurface {:x} = {:?}",
                        msg.window,
                        id,
                        surface,
                    );
                    match surface {
                        None => {
                            self.unpaired_surfaces.insert(id, (msg.window, location));
                        }
                        Some(surface) => {
                            self.new_window(msg.window, surface, location);
                        }
                    }
                } else {
                    self.update_title_x11(msg.window);
                }
            }
            _ => {}
        }
        self.conn.flush()?;
        Ok(())
    }

    fn new_window(&mut self, window: Window, surface: WlSurface, location: Point<i32, Logical>) {
        debug!(
            self.log,
            "Matched X11 surface {:x?} to {:x?}", window, surface
        );

        if give_role(&surface, "x11_surface").is_err() {
            // It makes no sense to post a protocol error here since that would only kill Xwayland
            error!(self.log, "Surface {:x?} already has a role?!", surface);
            return;
        }
        self.update_title(&surface, window);
        let x11surface = X11Surface {
            surface,
            window,
            popup: self.is_window_popup(window),
        };
        self.window_map
            .borrow_mut()
            .insert(Kind::X11(x11surface), location);
    }

    fn update_title_x11(&mut self, window: Window) {
        let wl_surface = self.window_map.borrow_mut().find_x11_window(window);
        if let Some(surface) = wl_surface {
            if let Some(kind) = surface.get_surface() {
                self.update_title(&kind, window);
            }
        }
    }

    fn is_window_popup(&mut self, window: Window) -> bool {
        // TODO some X11 windows are popups. Need to treat them as such
        if let Ok(value) = self.conn.get_property(
            false,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            self.atoms.ATOM,
            0,
            1024,
        ) {
            let reply = value.reply();
            match reply {
                Ok(a) => {
                    if let Some(mut atom_number_list) = a.value32() {
                        let atom_number = atom_number_list.next().unwrap().clone();
                        if atom_number == self.atoms._NET_WM_WINDOW_TYPE_MENU {
                            return true;
                        }
                    }
                }
                Err(_b) => {}
            }
        }
        false
    }

    fn get_title(&mut self, window: Window) -> Option<String> {
        if let Some(title) =
            self.get_string(window, self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING)
        {
            if title.len() > 0 {
                return Some(title);
            }
        }
        if let Some(title) = self.get_string(window, self.atoms._NET_WM_NAME, self.atoms.STRING) {
            if title.len() > 0 {
                return Some(title);
            }
        }
        if let Some(title) = self.get_string(window, self.atoms.WM_NAME, self.atoms.UTF8_STRING) {
            if title.len() > 0 {
                return Some(title);
            }
        }
        if let Some(title) = self.get_string(window, self.atoms.WM_NAME, self.atoms.STRING) {
            if title.len() > 0 {
                return Some(title);
            }
        }
        if let Some(title) = self.get_string(window, self.atoms.XA_WM_NAME, self.atoms.STRING) {
            if title.len() > 0 {
                return Some(title);
            }
        }
        None
    }

    fn get_string(&mut self, window: Window, atom_name: u32, atom_type: u32) -> Option<String> {
        if let Ok(title) = String::from_utf8(
            self.conn
                .get_property(false, window, atom_name, atom_type, 0, 1024)
                .unwrap()
                .reply()
                .unwrap()
                .value
                .clone(),
        ) {
            return Some(title);
        }
        None
    }

    fn update_title(&mut self, surface: &WlSurface, window: Window) {
        let title = self.get_title(window);
        with_states(surface, |states| {
            if let Some(title) = title {
                states
                    .data_map
                    .insert_if_missing(|| Mutex::new(TitleContainer { title }));
            }
            states.data_map.insert_if_missing(|| {
                Mutex::new(X11Id {
                    window: window as u32,
                })
            });
        })
        .unwrap();
    }
}

// Called when a WlSurface commits.
pub fn commit_hook(surface: &WlSurface) {
    // Is this the Xwayland client?
    if let Some(client) = surface.as_ref().client() {
        if let Some(x11) = client.data_map().get::<Rc<RefCell<X11State>>>() {
            let mut inner = x11.borrow_mut();
            // Is the surface among the unpaired surfaces (see comment next to WL_SURFACE_ID
            // handling above)
            if let Some((window, location)) = inner.unpaired_surfaces.remove(&surface.as_ref().id())
            {
                inner.new_window(window, surface.clone(), location);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct X11Surface {
    surface: WlSurface,
    window: Window,
    popup: bool,
}

impl std::cmp::PartialEq for X11Surface {
    fn eq(&self, other: &Self) -> bool {
        self.alive() && other.alive() && self.surface == other.surface
    }
}

impl X11Surface {
    pub fn is_popup(&self) -> bool {
        self.popup
    }

    pub fn alive(&self) -> bool {
        self.surface.as_ref().is_alive()
    }

    pub fn get_surface(&self) -> Option<&WlSurface> {
        if self.alive() {
            Some(&self.surface)
        } else {
            None
        }
    }

    pub fn get_window(&self) -> Option<Window> {
        if self.alive() {
            Some(self.window.clone())
        } else {
            None
        }
    }
}
