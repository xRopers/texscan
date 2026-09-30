//! texscan-gui: a desktop front end for texscan-core, built with egui. It calls the
//! library directly and never shells out to the CLI.
//!
//! [`session`] holds the state and the slow operations, with no drawing code, so it can
//! be tested headlessly; [`jobs`] runs those operations off the UI thread; [`preview`]
//! and [`thumbs`] decode images in the background; [`app`] draws.

pub mod app;
pub mod jobs;
pub mod preview;
pub mod session;
pub mod thumbs;
pub mod widgets;

pub use app::App;
