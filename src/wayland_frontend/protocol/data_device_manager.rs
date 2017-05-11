// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! Implementations of Wayland `wl_data_device_manager` object.

use std::rc::Rc;

use skylane::server::{Bundle, Object, ObjectId, Task};
use skylane_protocols::server::Handler;
use skylane_protocols::server::wayland::wl_data_device_manager;

use global::Global;
use proxy::ProxyRef;

// -------------------------------------------------------------------------------------------------

/// Wayland `wl_data_device_manager` object.
struct DataDeviceManager {}

// -------------------------------------------------------------------------------------------------

pub fn get_global() -> Global {
    Global::new(wl_data_device_manager::NAME,
                wl_data_device_manager::VERSION,
                Rc::new(DataDeviceManager::new_object))
}

// -------------------------------------------------------------------------------------------------

impl DataDeviceManager {
    /// Creates new `DataDeviceManager`.
    fn new(_oid: ObjectId, _proxy_ref: ProxyRef) -> Self {
        DataDeviceManager {}
    }

    fn new_object(oid: ObjectId, _version: u32, proxy_ref: ProxyRef) -> Box<Object> {
        Box::new(Handler::<_, wl_data_device_manager::Dispatcher>::new(Self::new(oid, proxy_ref)))
    }
}

// -------------------------------------------------------------------------------------------------

#[allow(unused_variables)]
impl wl_data_device_manager::Interface for DataDeviceManager {
    fn create_data_source(&mut self,
                          this_object_id: ObjectId,
                          bundle: &mut Bundle,
                          id: ObjectId)
                          -> Task {
        // FIXME: Finish implementation of `create_data_source`.
        Task::None
    }

    fn get_data_device(&mut self,
                       this_object_id: ObjectId,
                       bundle: &mut Bundle,
                       id: ObjectId,
                       seat: ObjectId)
                       -> Task {
        // FIXME: Finish implementation of `get_data_device`.
        Task::None
    }
}

// -------------------------------------------------------------------------------------------------
