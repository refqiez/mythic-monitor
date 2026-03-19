mod interface;

pub use interface::RenderDataInterface;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as sys;

pub use sys::Renderer;