// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! This module provides device management functionality for Perceptia.

extern crate libudev;
extern crate nix;

#[macro_use]
extern crate timber;
#[macro_use]
extern crate qualia;
extern crate dharma;

pub mod device_manager_module;
pub use device_manager_module::DeviceManagerModule;

mod evdev;
mod drivers;
mod device_monitor;

pub mod udev;
