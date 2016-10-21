// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! `Exhibitor` manages tasks related to drawing and compositing surfaces.

// -------------------------------------------------------------------------------------------------

#![feature(deque_extras)]

extern crate dharma;
#[macro_use]
extern crate timber;
#[macro_use]
extern crate qualia;
extern crate frames;
extern crate output;

mod surface_history;
mod compositor;
mod pointer;
mod display;

// -------------------------------------------------------------------------------------------------

use std::rc::Rc;
use std::cell::RefCell;
use std::collections::HashMap;

use dharma::Signaler;
use qualia::{Coordinator, SurfaceId, Button, OptionalPosition, Position, Vector, Perceptron};
use output::Output;

use compositor::Compositor;
use pointer::Pointer;
use display::Display;

// -------------------------------------------------------------------------------------------------

/// `Exhibitor` manages tasks related to drawing and compositing surfaces.
pub struct Exhibitor {
    last_output_id: i32,
    compositor: Compositor,
    pointer: Rc<RefCell<Pointer>>,
    displays: HashMap<i32, Display>,
    coordinator: Coordinator,
    signaler: Signaler<Perceptron>,
}

// -------------------------------------------------------------------------------------------------

/// General methods.
impl Exhibitor {
    /// `Exhibitor` constructor.
    pub fn new(signaler: Signaler<Perceptron>, coordinator: Coordinator) -> Self {
        Exhibitor {
            last_output_id: 0,
            compositor: Compositor::new(coordinator.clone()),
            pointer: Rc::new(RefCell::new(Pointer::new(signaler.clone(), coordinator.clone()))),
            displays: HashMap::new(),
            coordinator: coordinator,
            signaler: signaler,
        }
    }
}

// -------------------------------------------------------------------------------------------------

/// Notification handlers.
impl Exhibitor {
    /// Handle notification about needed redraw.
    pub fn on_notify(&mut self) {
        for ref mut display in self.displays.values_mut() {
            display.on_notify();
        }
    }

    /// This method is called when new output was found.
    pub fn on_output_found(&mut self, bundle: qualia::DrmBundle) {
        log_info1!("Exhibitor: found output");
        let id = self.generate_next_output_id();
        let mut output = match Output::new(bundle, id) {
            Ok(output) => {
                log_info2!("Created output: {}", output.get_name());
                output
            }
            Err(err) => {
                log_error!("Could not create output: {}", err);
                return;
            }
        };
        log_info1!("Exhibitor: creating display");
        let display_frame = self.compositor.create_display(output.get_area(), output.get_name());
        let display = Display::new(self.coordinator.clone(),
                                   self.signaler.clone(),
                                   self.pointer.clone(),
                                   output,
                                   display_frame);
        self.displays.insert(id, display);
    }

    /// This method is called when pageflip occurred.
    /// `id` is ID of output that scheduled the pageflip.
    pub fn on_pageflip(&mut self, id: i32) {
        // Pass notification to associated display
        if let Some(ref mut display) = self.displays.get_mut(&id) {
            display.on_pageflip();
        }
    }

    /// This method is called when new surface is ready to be managed.
    pub fn on_surface_ready(&mut self, sid: SurfaceId) {
        self.compositor.manage_surface(sid);
    }

    /// This method is called when surface was destroyed.
    pub fn on_surface_destroyed(&mut self, sid: SurfaceId) {
        self.compositor.unmanage_surface(sid);
        self.pointer.borrow_mut().on_surface_destroyed(sid);
    }
}

// -------------------------------------------------------------------------------------------------

/// Input handlers.
impl Exhibitor {
    /// Handle pointer motion event.
    pub fn on_motion(&mut self, vector: Vector) {
        self.pointer.borrow_mut().move_and_cast(vector, &self.displays);
        self.coordinator.notify();
    }

    /// Handle pointer position event.
    pub fn on_position(&mut self, position: OptionalPosition) {
        self.pointer.borrow_mut().update_position(position, &self.displays);
        self.coordinator.notify();
    }

    /// Handle pointer button event.
    pub fn on_button(&self, button: Button) {}

    /// Handle pointer position reset event.
    pub fn on_position_reset(&self) {
        self.pointer.borrow_mut().reset_position()
    }
}

// -------------------------------------------------------------------------------------------------

/// Private methods.
impl Exhibitor {
    /// Generate next output ID.
    fn generate_next_output_id(&mut self) -> i32 {
        self.last_output_id += 1;
        self.last_output_id
    }
}

// -------------------------------------------------------------------------------------------------
