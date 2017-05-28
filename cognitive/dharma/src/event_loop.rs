// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! `EventLoop` organizes work flow of a single thread. Independent logical parts of application
//! called `Module`s can be freely added to given `EventLoop`s creating flexible multi-threading
//! framework. `EventLoop` contains one receiver which can be used to push events and data to event
//! queue.
//!
//! `Module`s are created inside new thread so do not have to implement Send. User passes only
//! their constructors to `EventLoopInfo` structure which is context for creation on `EventLoop`.
//!
//! If `EventLoop` is not enough or too much, one can make new loop by implementing `Service` trait.

// -------------------------------------------------------------------------------------------------

use std;
use std::collections::btree_map::BTreeMap as Map;

use bridge::{self, ReceiveResult, SpecialCommand};
use signaler;
use system;

// -------------------------------------------------------------------------------------------------

/// `Module` defines independent part of application. One module is bounded only to single thread.
/// More modules may be run in the same thread. Modules do not share memory, communicate with
/// signals.
pub trait Module {
    /// Type for transported packages.
    type T: Clone + Send;

    /// Type for context that will be passed to `Module` at initialization.
    type C: Clone + Send + Sync;

    /// Callback run just after start of `Module`.
    fn get_signals(&self) -> Vec<bridge::SignalId>;

    /// Callback run just after start of `Module`.
    fn initialize(&mut self);

    /// Callback run on every message `Module` subscribed for.
    fn execute(&mut self, package: &Self::T);

    /// Callback run just before termination of `Module`.
    fn finalize(&mut self);
}

// -------------------------------------------------------------------------------------------------

/// To avoid requirement for `Module` to be `Send` and `Sync` it is constructed by special trait
/// object.
///
/// NOTE: `ModuleConstructor` could be replaced by `FnBox` if it was stable.
pub trait ModuleConstructor: Send + Sync {
    /// Type for transported packages.
    type T: Clone + Send;

    /// Type for context that will be passed to `Module` at initialization.
    type C: Clone + Send + Sync;

    /// Constructs new `Module`.
    fn construct(&self, &mut Self::C) -> Box<Module<T = Self::T, C = Self::C>>;
}

// -------------------------------------------------------------------------------------------------

/// Trait for all `Service`s.
pub trait Service {
    /// Main loop for `Service`.
    fn run(&mut self);
}

// -------------------------------------------------------------------------------------------------

/// To avoid requirement for `Service` to be `Send` and `Sync` it is constructed by special trait
/// object.
///
/// NOTE: `ServiceConstructor` could be replaced by `FnBox` if it was stable.
pub trait ServiceConstructor: Send + Sync {
    /// Constructs new `Service`.
    fn construct(&self) -> Box<Service>;
}

// -------------------------------------------------------------------------------------------------

/// Context for creation of `Service`.
pub struct ServiceInfo {
    name: String,
    constructor: Box<ServiceConstructor>,
}

// -------------------------------------------------------------------------------------------------

impl ServiceInfo {
    /// Constructs new `ServiceInfo`.
    pub fn new(name: String, constructor: Box<ServiceConstructor>) -> Self {
        ServiceInfo {
            name: name,
            constructor: constructor,
        }
    }

    /// Consume `ServiceInfo` to start custom event loop in new thread.
    pub fn start(self) -> std::io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name(self.name.clone())
            .spawn(move || self.constructor.construct().run())
    }
}

// -------------------------------------------------------------------------------------------------

/// Context for creation of `EventLoop`.
pub struct EventLoopInfo<P, C>
    where P: Clone + Send + 'static,
          C: Clone + Send + Sync + 'static
{
    name: String,
    signaler: signaler::Signaler<P>,
    constructors: Vec<Box<ModuleConstructor<T = P, C = C>>>,
    context: C,
}

// -------------------------------------------------------------------------------------------------

impl<P, C> EventLoopInfo<P, C>
    where P: Clone + Send + std::fmt::Debug,
          C: Clone + Send + Sync
{
    /// Constructs new `EventLoopInfo`.
    pub fn new(name: String, signaler: signaler::Signaler<P>, context: C) -> Self {
        EventLoopInfo {
            name: name,
            signaler: signaler,
            constructors: Vec::new(),
            context: context,
        }
    }

    /// Add module constructor.
    pub fn add_module(&mut self, constructor: Box<ModuleConstructor<T = P, C = C>>) {
        self.constructors.push(constructor);
    }

    /// Consume `EventLoopInfo` to start event loop in new thread.
    pub fn start(self) -> std::io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name(self.name.clone())
            .spawn(move || EventLoop::new(self).run())
    }
}

// -------------------------------------------------------------------------------------------------

/// Thread loop with event queue with communication over `bridge`s.
pub struct EventLoop<P, C>
    where P: Clone + Send + 'static,
          C: Clone + Send + Sync
{
    signaler: signaler::Signaler<P>,
    modules: Vec<Box<Module<T = P, C = C>>>,
    receiver: bridge::Receiver<P>,
    subscriptions: Map<bridge::SignalId, Vec<usize>>,
}

// -------------------------------------------------------------------------------------------------

impl<P, C> EventLoop<P, C>
    where P: Clone + Send + std::fmt::Debug,
          C: Clone + Send + Sync
{
    /// `EventLoop` constructor. Constructs `EventLoop` and all assigned modules.
    pub fn new(mut info: EventLoopInfo<P, C>) -> Self {
        // Block chosen signals for this thread.
        system::block_signals();

        // Create event loop
        let mut event_loop = EventLoop {
            signaler: info.signaler,
            modules: Vec::new(),
            receiver: bridge::Receiver::new(),
            subscriptions: Map::new(),
        };

        // Consume constructors to return module instances
        loop {
            match info.constructors.pop() {
                Some(constructor) => {
                    event_loop.modules.push(constructor.construct(&mut info.context));
                }
                None => break,
            }
        }

        event_loop
    }

    /// Thread body.
    fn run(mut self) {
        self.initialize();
        self.do_run();
        self.finalize();
    }

    /// Helper method for initializing modules.
    fn initialize(&mut self) {
        self.signaler.register(&self.receiver);

        // Subscribe signals
        let mut i = 0;
        for m in self.modules.iter() {
            let signals = m.get_signals();
            for s in signals {
                if self.subscriptions.contains_key(&s) {
                    match self.subscriptions.get_mut(&s) {
                        Some(ref mut subscribers) => {
                            subscribers.push(i);
                        }
                        None => {} // FIXME warn
                    }
                } else {
                    self.subscriptions.insert(s, vec![i]);
                    self.signaler.subscribe(s, &self.receiver);
                }
            }
            i += 1;
        }

        // Initialize modules
        for mut m in self.modules.iter_mut() {
            m.initialize();
        }
    }

    /// Helper method implementing main loop of `EventLoop`.
    fn do_run(&mut self) {
        loop {
            match self.receiver.recv() {
                // Enum value used by `Signaler` to emit events.
                ReceiveResult::Defined(id, package) => {
                    match self.subscriptions.get_mut(&id) {
                        Some(ref mut subscribers) => {
                            // Inform all subscriber about notification.
                            for i in subscribers.iter() {
                                self.modules[*i].execute(&package);
                            }
                        }
                        None => {
                            // Received signal we did not subscribe for.
                            // Ignore it.
                        }
                    }
                }

                // Special command from `Signaler`.
                ReceiveResult::Special(command) => {
                    match command {
                        SpecialCommand::Terminate => break,
                    }
                }

                // Ignore, `Signaler` should never emit these.
                ReceiveResult::Plain(_) => {}
                ReceiveResult::Custom(_, _) => {}
                ReceiveResult::Any(_, _) => break,

                // Break in case of errors.
                ReceiveResult::Timeout => break,
                ReceiveResult::Empty => break,
                ReceiveResult::Err => break,
            }
        }
    }

    /// Helper method for finalizing modules.
    fn finalize(&mut self) {
        for mut m in self.modules.iter_mut() {
            m.finalize();
        }
    }
}

// -------------------------------------------------------------------------------------------------
