// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! This module provides functionality to implement main program loop by waiting for system events
//! using `epoll` mechanism.
//!
//! Source of events is represented by `EventHandler`s, while whole loop by `Dispatcher`s.
//! `LocalDispatcher` can be used for single-thread programs when `EventHandler` do not implement
//! `Send` while `Dispatcher` is meant for multi-threaded programs. `DispatcherController` and
//! `LocalDispatcherController` are used to control `Dispatcher` and `LocalDispatcher`
//! respectively.

// -------------------------------------------------------------------------------------------------

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::os::unix::io::RawFd;
use std::collections::HashMap;
use std::convert::From;

use nix;
use nix::sys::epoll;

use system;

// -------------------------------------------------------------------------------------------------

/// `epoll_wait` waits infinitely when passed negative number as timeout.
const WAIT_INFINITELY: isize = -1;

// -------------------------------------------------------------------------------------------------

/// This module contains flags defining kind of event.
pub mod event_kind {
    use std::convert::From;
    use nix::sys::epoll;

    bitflags!(
        /// Type defining event kind.
        pub flags EventKind: u32 {
            /// Defines read event.
            const READ = 0x1,
            /// Defines write event.
            const WRITE = 0x2,
            /// Defines error or hangup. Can be omitted when adding source. It will be subscribed
            /// even if not specified, so it has to be always handled by `EventHandler`
            /// implementation.
            const HANGUP = 0x4
        }
    );

    impl From<epoll::EpollFlags> for EventKind {
        fn from(flags: epoll::EpollFlags) -> Self {
            let mut result = EventKind::empty();
            if flags.intersects(epoll::EPOLLIN) {
                result.insert(READ);
            }
            if flags.intersects(epoll::EPOLLOUT) {
                result.insert(WRITE);
            }
            if flags.intersects(epoll::EPOLLERR | epoll::EPOLLHUP) {
                result.insert(HANGUP);
            }
            result
        }
    }

    impl Into<epoll::EpollFlags> for EventKind {
        fn into(self) -> epoll::EpollFlags {
            let mut result = epoll::EpollFlags::empty();
            if self.intersects(READ) {
                result.insert(epoll::EPOLLIN);
            }
            if self.intersects(WRITE) {
                result.insert(epoll::EPOLLOUT);
            }
            if self.intersects(HANGUP) {
                // Only for completeness. This is not necessary to pass these flags to `epoll_ctl`.
                result.insert(epoll::EPOLLERR | epoll::EPOLLHUP);
            }
            result
        }
    }
}

pub use event_kind::EventKind;

// -------------------------------------------------------------------------------------------------

/// Id of `EventHandler` (generated by `Dispatcher`).
pub type EventHandlerId = u64;

// -------------------------------------------------------------------------------------------------

/// Trait for all structures supposed to be handlers for events registered in `Dispatcher`.
/// `EventHandler` is responsible for processing events. `EventHandler::process_event` will be
/// called when handlers file descriptor becomes readable in thread where `Dispatcher::start` was
/// called.
pub trait EventHandler {
    /// Returns file descriptor.
    fn get_fd(&self) -> RawFd;

    /// Callback function executed on event received.
    fn process_event(&mut self, event_kind: EventKind);

    /// This method is called by `Dispatcher` right after adding this `EventHandler`. Passed value
    /// is newly assigned ID of `EventHandler` which can be later used to delete it from
    /// `Dispatcher`.
    fn set_id(&mut self, _id: EventHandlerId) {}
}

// -------------------------------------------------------------------------------------------------

/// Helper structure for storing part of state of `Dispatcher` and `LocalDispatcher` which must be
/// guarded by mutex.
struct InnerState<E>
    where E: EventHandler + ?Sized
{
    epfd: RawFd,
    last_id: EventHandlerId,
    handlers: HashMap<EventHandlerId, Box<E>>,
}

impl<E> InnerState<E>
    where E: EventHandler + ?Sized
{
    /// Constructs new `InnerState`.
    pub fn new() -> Self {
        InnerState {
            epfd: epoll::epoll_create().expect("Failed to create epoll!"),
            last_id: 0,
            handlers: HashMap::new(),
        }
    }

    /// Adds `EventHandler`.
    ///
    /// Returns ID assigned to the added `EventHandler` which can be used to later delete it.
    pub fn add_source(&mut self, mut source: Box<E>, event_kind: EventKind) -> EventHandlerId {
        self.last_id += 1;
        let id = self.last_id;
        source.set_id(id);
        let fd = source.get_fd();
        self.handlers.insert(id, source);

        let mut event = epoll::EpollEvent::new(event_kind.into(), id);
        epoll::epoll_ctl(self.epfd, epoll::EpollOp::EpollCtlAdd, fd, &mut event)
            .expect("Failed to perform `epoll_ctl`");

        id
    }

    /// Deletes `EventHandler`.
    pub fn delete_source(&mut self, id: EventHandlerId) -> Option<Box<E>> {
        let result = self.handlers.remove(&id);
        if let Some(ref handler) = result {
            let mut event = epoll::EpollEvent::new(epoll::EpollFlags::empty(), 0);
            epoll::epoll_ctl(self.epfd, epoll::EpollOp::EpollCtlDel, handler.get_fd(), &mut event)
                .expect("Failed to delete epoll source");
        }
        result
    }

    /// Passes execution of event to handler.
    ///
    /// If file descriptor hung up the corresponding handler is removed.
    pub fn process(&mut self, id: EventHandlerId, event_kind: EventKind) {
        if let Some(handler) = self.handlers.get_mut(&id) {
            handler.process_event(event_kind);
        }
        if event_kind == event_kind::HANGUP {
            self.delete_source(id);
        }
    }
}

// -------------------------------------------------------------------------------------------------

/// Helper structure for storing state of `Dispatcher` and `LocalDispatcher`.
struct State<E>
    where E: EventHandler + ?Sized
{
    state: Mutex<InnerState<E>>,
    run: AtomicBool,
}

impl<E> State<E>
    where E: EventHandler + ?Sized
{
    pub fn new() -> Self {
        State {
            state: Mutex::new(InnerState::new()),
            run: AtomicBool::new(false),
        }
    }
}

// -------------------------------------------------------------------------------------------------

/// Helper method for waiting for events and then processing them.
fn do_wait_and_process<E>(state: &mut Arc<State<E>>, epfd: RawFd, timeout: isize)
    where E: EventHandler + ?Sized
{
    // We will process epoll events one by one.
    let mut events: [epoll::EpollEvent; 1] =
        [epoll::EpollEvent::new(epoll::EpollFlags::empty(), 0)];

    let wait_result = epoll::epoll_wait(epfd, &mut events[0..1], timeout);

    match wait_result {
        Ok(ready) => {
            if ready > 0 {
                let id = &events[0].data();
                let event_kind = EventKind::from(events[0].events());
                state.state.lock().unwrap().process(*id, event_kind);
            }
        }
        Err(err) => {
            if let nix::Error::Sys(errno) = err {
                if errno != nix::Errno::EINTR {
                    panic!("Error occurred during processing epoll events! ({:?})", err);
                }
            }
        }
    }
}

/// Helper method for precessing events in infinite loop.
fn do_run<E>(state: &mut Arc<State<E>>, epfd: RawFd)
    where E: EventHandler + ?Sized
{
    // Initial setup
    system::block_signals();
    state.run.store(true, Ordering::Relaxed);

    // Main loop
    loop {
        do_wait_and_process(state, epfd, WAIT_INFINITELY);

        if !state.run.load(Ordering::Relaxed) {
            break;
        }
    }
}


// -------------------------------------------------------------------------------------------------

/// Structure representing dispatcher of system events for use in one-threaded program.
pub struct LocalDispatcher {
    state: Arc<State<EventHandler>>,
}

impl LocalDispatcher {
    /// Constructor new `LocalDispatcher`.
    pub fn new() -> Self {
        LocalDispatcher { state: Arc::new(State::new()) }
    }

    /// Return local controller.
    ///
    /// This controller does not implement `Send`.
    pub fn get_controller(&self) -> LocalDispatcherController {
        LocalDispatcherController { state: self.state.clone() }
    }

    /// Waits for events and processes first one.
    pub fn wait_and_process(&mut self, timeout: Option<usize>) {
        let timeout = if let Some(t) = timeout {
            t as isize
        } else {
            WAIT_INFINITELY
        };
        let epfd = self.state.state.lock().unwrap().epfd;
        do_wait_and_process(&mut self.state, epfd, timeout);
    }

    /// Adds `EventHandler`.
    ///
    /// Returns ID assigned to the added `EventHandler` which can be used to later delete it.
    pub fn add_source(&mut self,
                      source: Box<EventHandler>,
                      event_kind: EventKind)
                      -> EventHandlerId {
        self.state.state.lock().unwrap().add_source(source, event_kind)
    }

    /// Deletes `EventHandler`.
    pub fn delete_source(&mut self, id: EventHandlerId) -> Option<Box<EventHandler>> {
        self.state.state.lock().unwrap().delete_source(id)
    }

    /// Starts processing events in current thread.
    pub fn run(&mut self) {
        let epfd = self.state.state.lock().unwrap().epfd;
        do_run(&mut self.state, epfd);
    }

    /// Stops processing of events.
    pub fn stop(&self) {
        self.state.run.store(false, Ordering::Relaxed)
    }
}

// -------------------------------------------------------------------------------------------------

/// Structure representing dispatcher of system events for use in multi-threaded program.
///
/// This version of `Dispatcher` does not accept `EventHandler`s which are not `Send`.
pub struct Dispatcher {
    state: Arc<State<EventHandler + Send>>,
}

impl Dispatcher {
    /// Constructs new `Dispatcher`.
    pub fn new() -> Self {
        Dispatcher { state: Arc::new(State::new()) }
    }

    /// Return controller.
    pub fn get_controller(&self) -> DispatcherController {
        DispatcherController { state: self.state.clone() }
    }

    /// Starts processing events in current thread.
    pub fn run(&mut self) {
        let epfd = self.state.state.lock().unwrap().epfd;
        do_run(&mut self.state, epfd);
    }
}

// -------------------------------------------------------------------------------------------------

/// Helps controlling `LocalDispatcher`.
///
/// Does not allow to add or delete handlers. In one-threaded program this would be unsafe.
#[derive(Clone)]
pub struct LocalDispatcherController {
    state: Arc<State<EventHandler>>,
}

impl LocalDispatcherController {
    /// Stops processing events.
    pub fn stop(&self) {
        self.state.run.store(false, Ordering::Relaxed)
    }
}

// -------------------------------------------------------------------------------------------------

/// Helps controlling `Dispatcher`.
#[derive(Clone)]
pub struct DispatcherController {
    state: Arc<State<EventHandler + Send>>,
}

impl DispatcherController {
    /// Adds `EventHandler`.
    ///
    /// Returns ID assigned to the added `EventHandler` which can be used to later delete it.
    pub fn add_source(&mut self,
                      source: Box<EventHandler + Send>,
                      event_kind: EventKind)
                      -> EventHandlerId {
        self.state.state.lock().unwrap().add_source(source, event_kind)
    }

    /// Deletes `EventHandler`.
    pub fn delete_source(&mut self, id: EventHandlerId) -> Option<Box<EventHandler + Send>> {
        self.state.state.lock().unwrap().delete_source(id)
    }

    /// Stops processing events.
    pub fn stop(&self) {
        self.state.run.store(false, Ordering::Relaxed)
    }
}

// -------------------------------------------------------------------------------------------------
