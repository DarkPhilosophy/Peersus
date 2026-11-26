//! The optional raster backend, linked only into workers that render canvases, decode pictures or decode sounds.
#![forbid(unsafe_code)]

use peersus_plugin_runtime::{RunError, Transport, run_with_canvas};

mod sound;
mod streams;

pub fn run<T: Transport>(bytes: &[u8], transport: &mut T) -> Result<(), RunError> {
    run_with_canvas(bytes, transport, &mut streams::Streams::new())
}
