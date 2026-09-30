//! texscan-core: find textures (DDS, and more to come) inside arbitrary binary files,
//! extract them, and later reinject edited versions.
//!
//! The typical flow is [`input::open`] → [`scan()`] → [`Manifest::new`] → [`extract_all`].
//! [`decode()`] turns one image of a texture into RGBA for previews and PNG export; after
//! editing, [`load_edits`] → [`pack()`] → [`PackResult::write_file`] puts textures back.
//! Front ends (CLI, later a GUI) call this library directly.

pub mod decode;
pub mod encode;
pub mod error;
pub mod export;
pub mod extract;
pub mod format;
pub mod formats;
pub mod input;
pub mod manifest;
pub mod output;
pub mod pack;
pub mod pixel;
pub mod scan;

pub use decode::{DecodeError, Image, Subresource, decode};
pub use encode::{EncodeError, encode, mip_chain};
pub use error::{Error, Result};
pub use export::{decode_png, encode_png, png_images, write_pngs};
pub use extract::{ExtractOptions, ExtractedFile, extract_all, texture_bytes};
pub use format::{Container, Orientation, Reject, Storage, TextureFormat, TextureInfo, format_for};
pub use manifest::{Manifest, SourceInfo, TextureEntry};
pub use pack::{Edits, FoundEdits, Outcome, PackOptions, PackResult, TextureEdit, TexturePlan, load_edits, pack, pack_texture};
pub use pixel::{Decode, Layout, PixelFormat};
pub use scan::{FoundTexture, Rejected, ScanOptions, ScanReport, scan, texture_at};
