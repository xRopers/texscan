//! texscan-core: find textures (DDS, and more to come) inside arbitrary binary files,
//! extract them, and later reinject edited versions.
//!
//! The typical flow is [`input::open`] → [`scan()`] → [`Manifest::new`] → [`extract_all`].
//! Front ends (CLI, later a GUI) call this library directly.

pub mod error;
pub mod extract;
pub mod format;
pub mod formats;
pub mod input;
pub mod manifest;
pub mod pixel;
pub mod scan;

pub use error::{Error, Result};
pub use extract::{ExtractOptions, ExtractedFile, extract_all, texture_bytes};
pub use format::{Container, Reject, TextureFormat, TextureInfo, format_for};
pub use manifest::{Manifest, SourceInfo, TextureEntry};
pub use pixel::{Layout, PixelFormat};
pub use scan::{FoundTexture, Rejected, ScanOptions, ScanReport, scan, texture_at};
