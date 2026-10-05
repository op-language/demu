//! EBML and Matroska reader and writer for the demu media pipeline.
//!
//! The crate implements three layers:
//!
//! - [`ebml`] — the EBML element primitives.
//! - [`ffv1`] — the FFV1 lossless video codec for the render form.
//! - [`matroska`] — the muxer and demuxer: the input form carries the
//!   input-script events with their exact domain stamps plus the system
//!   configuration as a tag and an attachment; the render form carries
//!   FFV1 video at one block per emulated display frame, a native-rate
//!   PCM track, and a resampled 48 kHz PCM track as the default audio
//!   track.

pub mod ebml;
pub mod ffv1;
pub mod matroska;

pub use matroska::{
    Cluster, InputFile, MkvError, RenderWriter, read_clusters, read_input_file, write_input_file,
};
