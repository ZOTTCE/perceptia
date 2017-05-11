// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! This module contains code responsible for gluing all other parts of crate together.

// -------------------------------------------------------------------------------------------------
use std;
use std::collections::HashMap;

use dharma;
use skylane::server as wl;

use qualia::{Axis, Button, DrmBundle, Milliseconds, OutputInfo, Position, Size};
use qualia::{Key, KeyMods, KeyboardConfig, KeyboardState, Perceptron, Settings};
use qualia::{surface_state, SurfaceId, SurfaceFocusing};
use coordination::Coordinator;

use protocol;
use gateway::Gateway;
use proxy::{Proxy, ProxyRef};
use mediator::{Mediator, MediatorRef};
use event_handlers::{ClientEventHandler, DisplayEventHandler};
use std::path::PathBuf;

// -------------------------------------------------------------------------------------------------

/// Helper structure for aggregating `Connection` with its `Proxy`.
struct Client {
    connection: wl::Connection,
    proxy: ProxyRef,
}

// -------------------------------------------------------------------------------------------------

/// This is main structure of `wayland_frontend` crate.
///
/// For information about its role and place among other structures see crate-level documentation.
pub struct Engine {
    display: wl::DisplaySocket,
    mediator: MediatorRef,
    clients: HashMap<dharma::EventHandlerId, Client>,
    output_infos: Vec<OutputInfo>,
    coordinator: Coordinator,
    settings: Settings,
    dispatcher: dharma::LocalDispatcher,
    keyboard_state: KeyboardState,
}

// -------------------------------------------------------------------------------------------------

impl Engine {
    /// Creates new `Engine`. Sets display socket up.
    pub fn new(coordinator: Coordinator,
               settings: Settings,
               keyboard_config: KeyboardConfig) -> Self {
        let mut partial_socket_path = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR must be defined!"));
        let mut socket = None;
        for i in 0..10{
            partial_socket_path.push(format!("wayland-{}", i));
            let try_sock = wl::DisplaySocket::new(&partial_socket_path);
            if let Ok(sock) = try_sock{
                socket = Some(sock);
                break;
            }
            partial_socket_path.pop();
        }
        Engine {
            display: socket.expect("wayland engine ERROR: cannot create a DisplaySocket."),
            mediator: MediatorRef::new(Mediator::new()),
            clients: HashMap::new(),
            output_infos: Vec::new(),
            coordinator: coordinator,
            settings: settings,
            dispatcher: dharma::LocalDispatcher::new(),
            keyboard_state: KeyboardState::new(&keyboard_config).expect("Creating keyboard state"),
        }
    }

    /// Starts `Engine`: adds display socket to `LocalDispatcher`.
    pub fn start(&mut self, sender: dharma::Sender<Perceptron>) {
        let handler = Box::new(DisplayEventHandler::new(self.display.clone(), sender));
        self.dispatcher.add_source(handler, dharma::event_kind::READ);
    }

    /// Reads client requests without blocking.
    pub fn receive(&mut self) {
        self.dispatcher.wait_and_process(Some(0));
    }
}

// -------------------------------------------------------------------------------------------------

/// Public handlers for client related events.
impl Engine {
    /// Handles new client:
    /// - accepts socket and adds it to `Dispatcher`
    /// - creates proxy for new client and registers global Wayland objects.
    /// - creates global display Wayland objects and bind it to client
    pub fn handle_new_client(&mut self, sender: dharma::DirectSender<Perceptron>) {
        // Accept the client.
        let mut client_socket = self.display.accept().expect("Accepting client");
        client_socket.set_logger(Some(Self::logger));

        // Prepare event handler.
        let id = self.dispatcher
            .add_source(Box::new(ClientEventHandler::new(client_socket.clone(), sender)),
                        dharma::event_kind::READ);

        // Prepare proxy.
        let mut proxy = Proxy::new(id,
                                   self.coordinator.clone(),
                                   self.settings.clone(),
                                   self.mediator.clone(),
                                   client_socket.clone());
        proxy.register_global(protocol::shm::get_global());
        proxy.register_global(protocol::compositor::get_global());
        proxy.register_global(protocol::shell::get_global());
        proxy.register_global(protocol::xdg_shell_v6::get_global());
        proxy.register_global(protocol::data_device_manager::get_global());
        proxy.register_global(protocol::seat::get_global());
        proxy.register_global(protocol::subcompositor::get_global());
        proxy.register_global(protocol::weston_screenshooter::get_global());
        proxy.register_global(protocol::linux_dmabuf_v1::get_global());
        proxy.register_global(protocol::mesa_drm::get_global());
        for info in self.output_infos.iter() {
            proxy.register_global(protocol::output::get_global(info.clone()));
        }
        let proxy_ref = ProxyRef::new(proxy);

        // Prepare client.
        let display = protocol::display::Display::new_object(proxy_ref.clone());
        let mut connection = wl::Connection::new(client_socket);
        connection.add_object(wl::DISPLAY_ID, display);
        let client = Client {
            connection: connection,
            proxy: proxy_ref,
        };
        self.clients.insert(id, client);
    }

    /// Handles termination (socket hung up) of client.
    pub fn terminate_client(&mut self, id: dharma::EventHandlerId) {
        let result1 = if let Some(_handler) = self.dispatcher.delete_source(id) {
            true
        } else {
            log_warn2!("Dispatching handler not found for client {} on termination", id);
            false
        };

        let result2 = if let Some(_client) = self.clients.remove(&id) {
            true
        } else {
            log_warn2!("Proxy not found for client {} on termination", id);
            false
        };

        if result1 && result2 {
            log_wayl3!("Client {} terminated successfully", id);
        }
    }

    /// Handles request from client associated with given `id`.
    pub fn process_events(&mut self, id: dharma::EventHandlerId) {
        if let Some(ref mut client) = self.clients.get_mut(&id) {
            if let Err(err) = client.connection.process_events() {
                log_warn3!("Wayland Engine: ERROR: {:?}", err);
            }
        } else {
            log_warn1!("Wayland Engine: No client: {}", id);
        }
    }
}

// -------------------------------------------------------------------------------------------------

/// Private helper methods.
impl Engine {
    fn logger(s: String) {
        log_wayl4!("Skylane: {}", s);
    }
}

// -------------------------------------------------------------------------------------------------

impl Gateway for Engine {
    fn on_output_found(&mut self, bundle: DrmBundle) {
        self.mediator.borrow_mut().set_drm_device(bundle.fd, bundle.path);
    }

    fn on_display_created(&mut self, output_info: OutputInfo) {
        self.output_infos.push(output_info.clone());
        for (_, client) in self.clients.iter() {
            client.proxy.borrow_mut().on_display_created(output_info.clone());
        }
    }

    fn on_keyboard_input(&mut self, key: Key, _mods: Option<KeyMods>) {
        let mods = if self.keyboard_state.update(key.code, key.value) {
            Some(self.keyboard_state.get_mods())
        } else {
            None
        };

        let sid = self.coordinator.get_keyboard_focused_sid();
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow_mut().on_keyboard_input(key, mods);
            }
        }
    }

    fn on_surface_frame(&mut self, sid: SurfaceId, milliseconds: Milliseconds) {
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow_mut().on_surface_frame(sid, milliseconds);
            }
        }
    }

    fn on_pointer_focus_changed(&self,
                                old_sid: SurfaceId,
                                new_sid: SurfaceId,
                                position: Position) {
        let mediator = self.mediator.borrow();
        let old_client_id = mediator.get_client_for_sid(old_sid);
        let new_client_id = mediator.get_client_for_sid(new_sid);

        if new_client_id != old_client_id {
            if let Some(client_id) = old_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_pointer_focus_changed(old_sid,
                                                                       SurfaceId::invalid(),
                                                                       Position::default());
                }
            }
            if let Some(client_id) = new_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_pointer_focus_changed(SurfaceId::invalid(),
                                                                       new_sid,
                                                                       position);
                }
            }
        } else {
            if let Some(client_id) = old_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_pointer_focus_changed(old_sid, new_sid, position);
                }
            }
        }
    }

    fn on_pointer_relative_motion(&self,
                                  sid: SurfaceId,
                                  position: Position,
                                  milliseconds: Milliseconds) {
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow_mut().on_pointer_relative_motion(sid, position, milliseconds);
            }
        }
    }

    fn on_pointer_button(&self, btn: Button) {
        let sid = self.coordinator.get_pointer_focused_sid();
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow_mut().on_pointer_button(btn);
            }
        }
    }

    fn on_pointer_axis(&self, axis: Axis) {
        let sid = self.coordinator.get_pointer_focused_sid();
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow_mut().on_pointer_axis(axis);
            }
        }
    }

    fn on_keyboard_focus_changed(&mut self, old_sid: SurfaceId, new_sid: SurfaceId) {
        let mediator = self.mediator.borrow();
        let old_client_id = mediator.get_client_for_sid(old_sid);
        let new_client_id = mediator.get_client_for_sid(new_sid);

        if new_client_id != old_client_id {
            if let Some(client_id) = old_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_keyboard_focus_changed(old_sid,
                                                                        SurfaceId::invalid());
                }
            }
            if let Some(client_id) = new_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_keyboard_focus_changed(SurfaceId::invalid(),
                                                                        new_sid);
                }
            }
        } else {
            if let Some(client_id) = old_client_id {
                if let Some(client) = self.clients.get(&client_id) {
                    client.proxy.borrow_mut().on_keyboard_focus_changed(old_sid, new_sid);
                }
            }
        }
    }

    fn on_surface_reconfigured(&self,
                               sid: SurfaceId,
                               size: Size,
                               state_flags: surface_state::SurfaceState) {
        if let Some(id) = self.mediator.borrow().get_client_for_sid(sid) {
            if let Some(client) = self.clients.get(&id) {
                client.proxy.borrow().on_surface_reconfigured(sid, size, state_flags);
            }
        }
    }

    fn on_screenshot_done(&mut self) {
        if let Some(id) = {
            let mediator = self.mediator.borrow();
            mediator.get_screenshooter()
        } {
            if let Some(client) = self.clients.get_mut(&id) {
                client.proxy.borrow_mut().on_screenshot_done();
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
