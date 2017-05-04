// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! This module contains interface for all output devices or mocks.

use qualia::{Buffer, Illusion, OutputInfo, Position, SurfaceContext, SurfaceViewer};

// -------------------------------------------------------------------------------------------------

/// `Output` is representation of physical output device.
pub trait Output {
    /// Draws passed scene using renderer.
    fn draw(&mut self,
            layunder: &Vec<SurfaceContext>,
            surfaces: &Vec<SurfaceContext>,
            layover: &Vec<SurfaceContext>,
            viewer: &SurfaceViewer)
            -> Result<(), Illusion>;

    /// Takes screenshot. Returns `Buffer` containing image data.
    fn take_screenshot(&self) -> Result<Buffer, Illusion>;

    /// Returns info about output.
    fn get_info(&self) -> OutputInfo;

    /// Sets global position.
    fn set_position(&mut self, position: Position);

    /// Swaps buffers.
    fn swap_buffers(&mut self) -> Result<u32, Illusion>;

    /// Schedules pageflip. Handler is registered by `DeviceManager`.
    fn schedule_pageflip(&self) -> Result<(), Illusion>;

    /// Reinitializes the output.
    fn recreate(&self) -> Result<Box<Output>, Illusion>;
}

// -------------------------------------------------------------------------------------------------
