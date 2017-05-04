// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of
// the MPL was not distributed with this file, You can obtain one at http://mozilla.org/MPL/2.0/

//! This module contains code dedicated to managing DRM output device like buffer swapping or
//! controlling v-blanks.

// -------------------------------------------------------------------------------------------------

use libgbm;
use libdrm::drm_mode;
use std::collections::HashMap;
use std::collections::VecDeque;

use qualia::{Buffer, DrmBundle, Illusion, SurfaceContext, SurfaceViewer};
use qualia::{Area, OutputInfo, Position, Size};
use renderer_gl::{egl_tools, RendererGl};

use gbm_tools::GbmBucket;
use output::Output;

// -------------------------------------------------------------------------------------------------

const INVALID_FRAMEBUFFER: u32 = 0;

// -------------------------------------------------------------------------------------------------

/// `DrmOutput` is representation of physical output device.
pub struct DrmOutput {
    /// Global position.
    position: Position,

    /// Size of the output in pixels.
    size: Size,

    /// Size of the output in millimeters.
    physical_size: Size,

    /// Id of the output. Guarantied to be unique in application.
    id: i32,

    /// Name of the output.
    name: String,

    /// Map from Buffer Object handle to Framebuffer id.
    buffers: HashMap<u32, u32>,

    /// Collection of GBM-related data.
    gbm: GbmBucket,

    /// Collection of DRM-related data.
    drm: DrmBundle,

    /// DRM mode.
    mode: drm_mode::ModeInfo,

    /// Renderer.
    renderer: RendererGl,

    /// Container for Buffer Objects.
    ///
    /// NOTE: This does not have to be vector. We only need one buffer at a time. Container was
    /// introduced to satisfy borrow checker.
    bo: VecDeque<libgbm::BufferObject>,

    /// Current framebuffer id.
    fb: u32,
}

// -------------------------------------------------------------------------------------------------

impl DrmOutput {
    /// Constructs new `DrmOutput`.
    pub fn new(drm: DrmBundle, id: i32) -> Result<Box<Output>, Illusion> {
        // Get size
        let mode;
        let size;
        let modes;
        let physical_size;
        if let Some(connector) = drm_mode::get_connector(drm.fd, drm.connector_id) {
            modes = connector.get_modes();
            mode = modes.get(0).unwrap().clone();
            size = Size::new(mode.get_hdisplay() as usize, mode.get_vdisplay() as usize);
            physical_size = Size::new(connector.get_mm_width() as usize,
                                      connector.get_mm_height() as usize);
        } else {
            return Err(Illusion::General(format!("Failed to get mode for connector")));
        }

        // GBM
        let gbm = GbmBucket::new(drm.fd, size.clone())?;

        // EGL
        let egl = egl_tools::EglBucket::new(gbm.device.c_struct() as *mut _,
                                            gbm.surface.c_struct() as *mut _)?;

        // Create renderer
        let renderer = RendererGl::new(egl, size.clone());

        // Create output
        let mut mine = DrmOutput {
            id: id,
            position: Position::default(),
            size: size,
            physical_size: physical_size,
            name: "".to_owned(),
            renderer: renderer,
            mode: mode,
            drm: drm,
            gbm: gbm,
            buffers: HashMap::new(),
            bo: VecDeque::with_capacity(1),
            fb: INVALID_FRAMEBUFFER,
        };

        // Initialize renderer
        mine.renderer.initialize()?;
        mine.swap_buffers()?;

        Ok(Box::new(mine))
    }
}

// -------------------------------------------------------------------------------------------------

// Public methods
impl Output for DrmOutput {
    /// Draws passed scene using renderer.
    fn draw(&mut self,
            layunder: &Vec<SurfaceContext>,
            surfaces: &Vec<SurfaceContext>,
            layover: &Vec<SurfaceContext>,
            viewer: &SurfaceViewer)
            -> Result<(), Illusion> {
        self.renderer.draw(layunder, surfaces, layover, viewer)
    }

    /// Takes screenshot. Returns `Buffer` containing image data.
    fn take_screenshot(&self) -> Result<Buffer, Illusion> {
        self.renderer.take_screenshot()
    }

    /// Returns info about output.
    fn get_info(&self) -> OutputInfo {
        // TODO: Make Output aware of its position.
        let area = Area::new(self.position, self.size);

        OutputInfo::new(self.id,
                        area,
                        self.physical_size,
                        60, // TODO: make output aware of its refresh rate.
                        self.name.clone(),
                        self.name.clone())
    }

    /// Sets global position.
    fn set_position(&mut self, position: Position) {
        self.position = position;
    }

    /// Swaps renderers and devices buffers.
    fn swap_buffers(&mut self) -> Result<u32, Illusion> {
        self.renderer.swap_buffers()?;
        self.swap_gbm_buffers()
    }

    /// Schedules pageflip. Handler is registered by `DeviceManager`.
    fn schedule_pageflip(&self) -> Result<(), Illusion> {
        match drm_mode::page_flip(self.drm.fd,
                                  self.drm.crtc_id,
                                  self.fb,
                                  drm_mode::PAGE_FLIP_EVENT,
                                  self.id) {
            Ok(_) => Ok(()),
            Err(err) => {
                let text = format!("Failed to page flip (crtc_id: {}, connector_id: {}, error: {})",
                                   self.drm.crtc_id,
                                   self.drm.connector_id,
                                   err);
                Err(Illusion::General(text))
            }
        }
    }

    /// Reinitializes the output.
    fn recreate(&self) -> Result<Box<Output>, Illusion> {
        DrmOutput::new(self.drm, self.id)
    }

}

// -------------------------------------------------------------------------------------------------

// Private methods
impl DrmOutput {
    /// Swap device buffers.
    /// Create buffer if necessary.
    fn swap_gbm_buffers(&mut self) -> Result<u32, Illusion> {
        if let Some(bo) = self.bo.pop_front() {
            self.gbm.surface.release_buffer(bo);
        }

        if let Some(bo) = self.gbm.surface.lock_front_buffer() {
            let width = bo.width();
            let height = bo.height();
            let stride = bo.stride();
            let handle = bo.handle_u32();
            self.bo.push_back(bo);
            if self.buffers.contains_key(&handle) {
                self.fb = *self.buffers.get(&handle).unwrap();
                Ok(self.fb)
            } else {
                match drm_mode::add_fb(self.drm.fd, width, height, 24, 32, stride, handle) {
                    Ok(fb) => {
                        match drm_mode::set_crtc(self.drm.fd,
                                                 self.drm.crtc_id,
                                                 fb,
                                                 0,
                                                 0,
                                                 &[self.drm.connector_id],
                                                 &self.mode) {
                            Ok(_) => {
                                self.buffers.insert(handle, fb);
                                self.fb = fb;
                                Ok(fb)
                            }
                            Err(_) => Err(Illusion::General(format!("Failed to set CRTC"))),
                        }
                    }
                    Err(_) => Err(Illusion::General(format!("Failed to create DRM framebuffer"))),
                }
            }
        } else {
            Err(Illusion::General(format!("Failed to lock front buffer")))
        }
    }
}

// -------------------------------------------------------------------------------------------------
