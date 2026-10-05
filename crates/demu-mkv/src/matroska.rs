//! The Matroska muxer and demuxer over the EBML primitives.
//!
//! Two file forms exist. The input form carries one metadata track whose
//! blocks encode the input events with their exact domain stamps, plus
//! the system configuration as a tag and a file attachment. The render
//! form carries the FFV1 video track at one block per emulated display
//! frame, a native-rate PCM track, and the resampled 48 kHz PCM track as
//! the default-selected audio track.
//!
//! The timestamp scale is one nanosecond, and each video frame occupies
//! one cluster whose timestamp is the frame's exact nanosecond time, so
//! frame durations derive from the machine frame clock without rounding.
//! The form is demu's own; the decoder of a foreign file follows the
//! same element rules and ignores what it does not declare.

use std::io::Write;
use std::path::Path;

use demu_config::model::SystemConfig;
use demu_core::frequency::Frequency;
use demu_core::script::ScriptEvent;

use crate::ebml;
use crate::ffv1;

/// The timestamp scale: one nanosecond per timestamp tick.
const TIMESTAMP_SCALE: u64 = 1;

/// The codec identifier of the FFV1 video track.
const CODEC_VIDEO: &str = "V_FFV1";

/// The codec identifier of the little-endian 16-bit PCM audio tracks.
const CODEC_PCM: &str = "A_PCM/INT/LIT";

/// The codec identifier of demu's private input-event track.
const CODEC_INPUT: &str = "D_DEMU/INPUT/1";

/// The track number of the input-event track and of the resampled 48
/// kHz audio track.
const TRACK_PRIMARY: u64 = 1;

/// The track number of the video track.
const TRACK_VIDEO: u64 = 2;

/// The track number of the native-rate audio track.
const TRACK_NATIVE: u64 = 3;

/// The file name of the attached system configuration.
const CONFIG_ATTACHMENT: &str = "config.toml";

/// The tag name carrying the system configuration text.
const TAG_CONFIG: &str = "demu.config";

/// The tag name carrying the target triple.
const TAG_TRIPLE: &str = "demu.triple";

/// The tag name carrying the machine name.
const TAG_MACHINE: &str = "demu.machine";

/// The input-event payload magic in the private codec data.
const INPUT_MAGIC: &[u8; 8] = b"DEMUINP1";

/// A Matroska layer failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MkvError {
    /// The file operation or a byte write failed.
    #[error("io error: {0}")]
    Io(String),
    /// The stream is not an EBML Matroska file.
    #[error("not a Matroska file: {0}")]
    Format(String),
    /// The file carries no embedded system configuration.
    #[error("the file carries no system configuration")]
    NoConfig,
    /// An input-event payload is malformed.
    #[error("invalid input event payload: {0}")]
    Event(String),
    /// The FFV1 codec layer failed.
    #[error("video codec: {0}")]
    Ffv1(#[from] ffv1::Ffv1Error),
}

impl From<std::io::Error> for MkvError {
    fn from(e: std::io::Error) -> Self {
        MkvError::Io(e.to_string())
    }
}

/// The content of one input-form Matroska file.
#[derive(Debug, Clone, Default)]
pub struct InputFile {
    /// The system configuration text, from the tag or the attachment.
    pub config_text: Option<String>,
    /// The target triple tag, when present.
    pub triple: Option<String>,
    /// The machine name tag, when present.
    pub name: Option<String>,
    /// The input events in file order, with their exact domain stamps.
    pub events: Vec<ScriptEvent>,
}

/// Write the input form: one metadata track of event blocks plus the
/// configuration tags and attachment.
///
/// Each event block carries the exact domain name and tick count, so a
/// replay reproduces the script's timing without a nanosecond round
/// trip; the block timestamp is informational.
///
/// # Errors
///
/// Returns [`MkvError`] when the file cannot be written or an event
/// names a domain outside `config`.
pub fn write_input_file(
    path: &Path,
    config: &SystemConfig,
    config_text: &str,
    events: &[ScriptEvent],
) -> Result<(), MkvError> {
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);

    let mut header = Vec::new();
    write_ebml_header(&mut header)?;
    ebml::element(&mut out, 0x1A45DFA3, &header)?;

    let mut segment = Vec::new();
    segment.extend_from_slice(&info_element()?);
    segment.extend_from_slice(&input_tracks_element()?);
    segment.extend_from_slice(&tags_element(config_text, &config.triple, &config.name)?);
    segment.extend_from_slice(&attachment_element(config_text)?);
    for event in events {
        let domain = config.clocks.get(&event.domain).ok_or_else(|| {
            MkvError::Event(format!(
                "line {}: unknown domain `{}`",
                event.line, event.domain
            ))
        })?;
        let frequency = Frequency::new(domain.frequency.num(), domain.frequency.den())
            .map_err(|e| MkvError::Event(format!("line {}: bad domain: {e}", event.line)))?;
        let ns = frequency.ticks_to_ns(event.ticks);
        segment.extend_from_slice(&input_cluster(ns, event)?);
    }
    ebml::element(&mut out, 0x18538067, &segment)?;
    out.flush()?;
    Ok(())
}

/// The `Info` element with the nanosecond timestamp scale.
fn info_element() -> Result<Vec<u8>, MkvError> {
    let mut payload = Vec::new();
    ebml::uint_element(&mut payload, 0x2AD7B1, TIMESTAMP_SCALE)?;
    ebml::string_element(&mut payload, 0x4D80, env!("CARGO_PKG_VERSION"))?;
    ebml::string_element(&mut payload, 0x5741, "demu")?;
    let mut out = Vec::new();
    ebml::element(&mut out, 0x1549A966, &payload)?;
    Ok(out)
}

/// The `Tracks` element of the input form: one metadata track.
fn input_tracks_element() -> Result<Vec<u8>, MkvError> {
    let mut entry = Vec::new();
    ebml::uint_element(&mut entry, 0xD7, TRACK_PRIMARY)?;
    ebml::uint_element(&mut entry, 0x73C5, 0xDE10_0001)?;
    ebml::uint_element(&mut entry, 0x83, 0x21)?; // TrackType: metadata
    ebml::string_element(&mut entry, 0x86, CODEC_INPUT)?;
    ebml::binary_element(&mut entry, 0x63A2, INPUT_MAGIC)?;
    let mut tracks = Vec::new();
    ebml::element(&mut tracks, 0x1654AE6B, &entry)?;
    Ok(tracks)
}

/// The `Tags` element: one tag set with the machine summary and the
/// configuration text.
fn tags_element(config_text: &str, triple: &str, name: &str) -> Result<Vec<u8>, MkvError> {
    let mut tag = Vec::new();
    simple_tag(&mut tag, TAG_TRIPLE, triple)?;
    simple_tag(&mut tag, TAG_MACHINE, name)?;
    simple_tag(&mut tag, TAG_CONFIG, config_text)?;
    let mut one_tag = Vec::new();
    ebml::element(&mut one_tag, 0x7373, &tag)?;
    let mut tags = Vec::new();
    ebml::element(&mut tags, 0x1254C367, &one_tag)?;
    Ok(tags)
}

/// One `SimpleTag`: the name and the string value.
fn simple_tag(tag: &mut Vec<u8>, tag_name: &str, tag_string: &str) -> Result<(), MkvError> {
    let mut simple = Vec::new();
    ebml::string_element(&mut simple, 0x45A3, tag_name)?;
    ebml::string_element(&mut simple, 0x4487, tag_string)?;
    ebml::element(tag, 0x67C8, &simple)?;
    Ok(())
}

/// Append the EBML unknown-size marker: the reserved all-ones 8-byte
/// VINT that keeps a streamed segment open until end of file.
fn write_unknown_size<W: Write>(out: &mut W) -> std::io::Result<()> {
    out.write_all(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
}

/// The `Attachments` element carrying the configuration file.
fn attachment_element(config_text: &str) -> Result<Vec<u8>, MkvError> {
    let mut file = Vec::new();
    ebml::string_element(&mut file, 0x467E, "demu system configuration")?;
    ebml::string_element(&mut file, 0x466E, CONFIG_ATTACHMENT)?;
    ebml::string_element(&mut file, 0x4660, "application/toml")?;
    ebml::binary_element(&mut file, 0x465C, config_text.as_bytes())?;
    let mut attachments = Vec::new();
    ebml::element(&mut attachments, 0x1941A469, &file)?;
    Ok(attachments)
}

/// One event cluster: the event payload as one block.
fn input_cluster(ns: u64, event: &ScriptEvent) -> Result<Vec<u8>, MkvError> {
    let payload = encode_event(event);
    let block = simple_block(TRACK_PRIMARY, 0, true, &payload);
    let mut cluster = Vec::new();
    ebml::uint_element(&mut cluster, 0xE7, ns)?;
    ebml::element(&mut cluster, 0xA3, &block)?;
    let mut out = Vec::new();
    ebml::element(&mut out, 0x1F43B675, &cluster)?;
    Ok(out)
}

/// Encode one event into the private input payload: `u16le` lengths and
/// UTF-8 bytes for the domain and the device, a `u64le` tick count, and
/// the event text.
fn encode_event(event: &ScriptEvent) -> Vec<u8> {
    let mut out = Vec::new();
    let domain = event.domain.as_bytes();
    out.extend_from_slice(
        &u16::try_from(domain.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(domain);
    out.extend_from_slice(&event.ticks.to_le_bytes());
    let device = event.device.as_bytes();
    out.extend_from_slice(
        &u16::try_from(device.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(device);
    let body = event.event.as_bytes();
    out.extend_from_slice(&u16::try_from(body.len()).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Decode one event payload.
fn decode_event(payload: &[u8]) -> Result<ScriptEvent, MkvError> {
    let mut cursor = 0usize;
    let domain_len = field_u16(payload, &mut cursor)?;
    let domain = field_text(payload, &mut cursor, usize::from(domain_len), "domain name")?;
    let ticks = {
        let bytes = read_slice(payload, &mut cursor, 8)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        u64::from_le_bytes(raw)
    };
    let device_len = field_u16(payload, &mut cursor)?;
    let device = field_text(payload, &mut cursor, usize::from(device_len), "device name")?;
    let event_len = field_u16(payload, &mut cursor)?;
    let event = field_text(payload, &mut cursor, usize::from(event_len), "event text")?;
    if cursor != payload.len() {
        return Err(MkvError::Event(
            "payload carries trailing bytes".to_string(),
        ));
    }
    Ok(ScriptEvent {
        domain,
        ticks,
        device,
        event,
        line: 0,
    })
}

/// Read one `u16le` field.
fn field_u16(payload: &[u8], cursor: &mut usize) -> Result<u16, MkvError> {
    let bytes = read_slice(payload, cursor, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Read one length-prefixed UTF-8 field.
fn field_text(
    payload: &[u8],
    cursor: &mut usize,
    len: usize,
    what: &str,
) -> Result<String, MkvError> {
    let bytes = read_slice(payload, cursor, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| MkvError::Event(format!("{what} is not UTF-8")))
}

/// Read `n` bytes at the cursor, advancing it; error when short.
fn read_slice<'a>(payload: &'a [u8], cursor: &mut usize, n: usize) -> Result<&'a [u8], MkvError> {
    let end = (*cursor)
        .checked_add(n)
        .filter(|end| *end <= payload.len())
        .ok_or_else(|| MkvError::Event("payload ends mid-field".to_string()))?;
    let slice = &payload[*cursor..end];
    *cursor = end;
    Ok(slice)
}

/// One `SimpleBlock` payload: the track number VINT, the signed
/// relative timestamp, the flags, and the block data.
fn simple_block(track: u64, relative_ns: i64, keyframe: bool, data: &[u8]) -> Vec<u8> {
    debug_assert!(track < 0x80, "one-byte track VINT form");
    let mut out = Vec::with_capacity(data.len() + 4);
    // A one-byte track VINT carries the marker bit and the number.
    out.push(0x80 | track as u8);
    out.extend_from_slice(&relative_of(relative_ns).to_be_bytes());
    let mut flags = 0u8;
    if keyframe {
        flags |= 0x80;
    }
    out.push(flags);
    out.extend_from_slice(data);
    out
}

/// The block relative timestamp: the field is signed 16-bit.
fn relative_of(relative_ns: i64) -> i16 {
    i16::try_from(relative_ns).unwrap_or(i16::MAX)
}

/// One decoded cluster: the timestamp and each block by track.
#[derive(Debug, Clone)]
pub struct Cluster {
    /// The cluster timestamp in nanoseconds.
    pub timestamp_ns: u64,
    /// The blocks of the cluster as `(track, payload)` in file order.
    pub blocks: Vec<(u64, Vec<u8>)>,
}

/// Read the whole file into its cluster list, in file order.
///
/// The reader walks the segment and flattens every cluster's blocks;
/// the caller decides which tracks it needs.
///
/// # Errors
///
/// Returns [`MkvError`] for a malformed file.
pub fn read_clusters(path: &Path) -> Result<Vec<Cluster>, MkvError> {
    let data = std::fs::read(path)?;
    let mut top = ebml::Reader::new(&data);
    // The first element is the EBML header; the second is the segment.
    let _header = top
        .next_element()
        .ok_or_else(|| MkvError::Format("no EBML header".to_string()))?;
    let segment = top
        .next_element()
        .ok_or_else(|| MkvError::Format("no segment".to_string()))?;
    if segment.id != 0x18538067 {
        return Err(MkvError::Format(
            "the second element is not a segment".to_string(),
        ));
    }
    let segment_bytes = segment.bytes(&data);
    let mut walker = ebml::Reader::new(segment_bytes);
    let mut clusters = Vec::new();
    while let Some(element) = walker.next_element() {
        if element.id != 0x1F43B675 {
            continue;
        }
        let payload = element.bytes(segment_bytes);
        let mut reader = ebml::Reader::new(payload);
        let mut timestamp_ns = 0u64;
        let mut blocks = Vec::new();
        while let Some(field) = reader.next_element() {
            match field.id {
                0xE7 => {
                    timestamp_ns = ebml::Reader::payload_uint(field.bytes(payload)).unwrap_or(0);
                }
                0xA3 => {
                    let block = field.bytes(payload);
                    let track = block
                        .first()
                        .map(|byte| u64::from(*byte & 0x7F))
                        .unwrap_or(0);
                    // `track | i16 timestamp | u8 flags | payload`
                    let body = block.get(4..).unwrap_or(&[]);
                    blocks.push((track, body.to_vec()));
                }
                _ => {}
            }
        }
        clusters.push(Cluster {
            timestamp_ns,
            blocks,
        });
    }
    Ok(clusters)
}

/// Read the input form: extract the configuration, the tags, and the
/// event blocks.
///
/// The configuration text comes from the tag; the attached
/// `config.toml` file is the fallback.
///
/// # Errors
///
/// Returns [`MkvError`] for a malformed file or payload.
pub fn read_input_file(path: &Path) -> Result<InputFile, MkvError> {
    let data = std::fs::read(path)?;
    let mut top = ebml::Reader::new(&data);
    let mut out = InputFile::default();
    let mut attachment_config: Option<Vec<u8>> = None;
    let _header = top
        .next_element()
        .ok_or_else(|| MkvError::Format("no EBML header".to_string()))?;
    let segment = top
        .next_element()
        .ok_or_else(|| MkvError::Format("no segment".to_string()))?;
    if segment.id != 0x18538067 {
        return Err(MkvError::Format(
            "the second element is not a segment".to_string(),
        ));
    }
    let segment_bytes = segment.bytes(&data);
    let mut walker = ebml::Reader::new(segment_bytes);
    while let Some(element) = walker.next_element() {
        match element.id {
            0x1254C367 => read_tags(segment_bytes, element, &mut out)?,
            0x1941A469 => attachment_config = read_attachment(segment_bytes, element),
            0x1F43B675 => read_cluster(segment_bytes, element, &mut out)?,
            _ => {}
        }
    }
    if out.config_text.is_none()
        && let Some(raw) = attachment_config
    {
        out.config_text = String::from_utf8(raw).ok();
    }
    Ok(out)
}

/// Read the `Tags` element into the file summary.
///
/// Each `Tag` holds `SimpleTag` children with the name and the string
/// value one level down.
fn read_tags(
    data: &[u8],
    tags_element: ebml::Element,
    out: &mut InputFile,
) -> Result<(), MkvError> {
    let payload = tags_element.bytes(data);
    let mut tags = ebml::Reader::new(payload);
    while let Some(tag) = tags.next_element() {
        if tag.id != 0x7373 {
            continue;
        }
        let tag_bytes = tag.bytes(payload);
        let mut fields = ebml::Reader::new(tag_bytes);
        while let Some(simple) = fields.next_element() {
            if simple.id != 0x67C8 {
                continue;
            }
            let simple_bytes = simple.bytes(tag_bytes);
            let mut values = ebml::Reader::new(simple_bytes);
            let mut name: Option<String> = None;
            while let Some(field) = values.next_element() {
                match field.id {
                    0x45A3 => {
                        name = String::from_utf8(field.bytes(simple_bytes).to_vec()).ok();
                    }
                    0x4487 => {
                        let value = String::from_utf8(field.bytes(simple_bytes).to_vec())
                            .map_err(|_| MkvError::Format("tag string is not UTF-8".to_string()))?;
                        match name.as_deref() {
                            Some(TAG_CONFIG) => out.config_text = Some(value),
                            Some(TAG_TRIPLE) => out.triple = Some(value),
                            Some(TAG_MACHINE) => out.name = Some(value),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Read the `Attachments` element; return the `config.toml` file bytes.
fn read_attachment(data: &[u8], attachments_element: ebml::Element) -> Option<Vec<u8>> {
    let payload = attachments_element.bytes(data);
    let mut attachments = ebml::Reader::new(payload);
    while let Some(file) = attachments.next_element() {
        if file.id != 0x61A7 {
            continue;
        }
        let file_bytes = file.bytes(payload);
        let mut fields = ebml::Reader::new(file_bytes);
        let mut name: Option<String> = None;
        let mut content: Option<Vec<u8>> = None;
        while let Some(field) = fields.next_element() {
            match field.id {
                0x466E => name = String::from_utf8(field.bytes(file_bytes).to_vec()).ok(),
                0x465C => content = Some(field.bytes(file_bytes).to_vec()),
                _ => {}
            }
        }
        if name.as_deref() == Some(CONFIG_ATTACHMENT) {
            return content;
        }
    }
    None
}

/// Read the clusters of the input track into events, in file order.
fn read_cluster(
    data: &[u8],
    cluster_element: ebml::Element,
    out: &mut InputFile,
) -> Result<(), MkvError> {
    let payload = cluster_element.bytes(data);
    let mut cluster = ebml::Reader::new(payload);
    while let Some(element) = cluster.next_element() {
        if element.id != 0xA3 {
            continue;
        }
        let block = element.bytes(payload);
        let track = block
            .first()
            .map(|byte| u64::from(*byte & 0x7F))
            .unwrap_or(0);
        if track != TRACK_PRIMARY {
            continue;
        }
        // `track | i16 timestamp | u8 flags | payload`
        let body = block
            .get(4..)
            .ok_or_else(|| MkvError::Format("block carries no payload".to_string()))?;
        out.events.push(decode_event(body)?);
    }
    Ok(())
}

/// The render-form writer: FFV1 video, native-rate PCM, and a 48 kHz
/// PCM track as the default-selected audio track.
///
/// Track numbers: `1` the resampled 48 kHz PCM (the default-selected
/// audio track), `2` the FFV1 video, `3` the native-rate PCM.
pub struct RenderWriter {
    file: std::io::BufWriter<std::fs::File>,
    width: u32,
    height: u32,
    audio: bool,
}

impl RenderWriter {
    /// Create the render form at `path`.
    ///
    /// The writer carries the configuration as a tag and an attachment,
    /// the video geometry, and, when `native_rate` is given, both PCM
    /// audio tracks.
    ///
    /// # Errors
    ///
    /// Returns [`MkvError`] when the file or the codec record fails.
    pub fn create(
        path: &Path,
        width: u32,
        height: u32,
        native_rate: Option<u32>,
        config_text: &str,
        triple: &str,
        name: &str,
    ) -> Result<Self, MkvError> {
        let record = ffv1::configuration_record();
        if !ffv1::is_compatible(&record) {
            return Err(MkvError::Format(
                "the codec configuration record is incompatible".to_string(),
            ));
        }
        let file = std::fs::File::create(path)?;
        let mut writer = Self {
            file: std::io::BufWriter::new(file),
            width,
            height,
            audio: native_rate.is_some(),
        };

        let mut header = Vec::new();
        write_ebml_header(&mut header)?;
        ebml::element(&mut writer.file, 0x1A45DFA3, &header)?;

        let mut tracks = Vec::new();
        if let Some(rate) = native_rate {
            tracks.extend_from_slice(&pcm_track_entry(TRACK_PRIMARY, 48_000, true)?);
            tracks.extend_from_slice(&pcm_track_entry(TRACK_NATIVE, rate, false)?);
        }
        let mut video = Vec::new();
        ebml::uint_element(&mut video, 0xB0, u64::from(width))?;
        ebml::uint_element(&mut video, 0xBA, u64::from(height))?;
        let mut video_entry = Vec::new();
        ebml::uint_element(&mut video_entry, 0xD7, TRACK_VIDEO)?;
        ebml::uint_element(&mut video_entry, 0x73C5, 0xDE10_0002)?;
        ebml::uint_element(&mut video_entry, 0x83, 1)?;
        ebml::string_element(&mut video_entry, 0x86, CODEC_VIDEO)?;
        ebml::binary_element(&mut video_entry, 0x63A2, &record)?;
        ebml::element(&mut video_entry, 0xE0, &video)?;
        ebml::element(&mut tracks, 0xAE, &video_entry)?;

        // The render segment streams frame clusters until close, so it
        // opens with the EBML unknown-size marker and every element
        // the writer appends is its child until end of file. The
        // reader falls back to "the rest of the buffer" for the
        // all-ones size.
        ebml::write_id(&mut writer.file, 0x18538067)?;
        write_unknown_size(&mut writer.file)?;
        writer.file.write_all(&info_element()?)?;
        ebml::element(&mut writer.file, 0x1654AE6B, &tracks)?;
        writer
            .file
            .write_all(&tags_element(config_text, triple, name)?)?;
        writer.file.write_all(&attachment_element(config_text)?)?;
        Ok(writer)
    }

    /// Append one emulated display frame: the FFV1 video block and both
    /// audio blocks of the frame at the frame's exact nanosecond time.
    ///
    /// # Errors
    ///
    /// Returns [`MkvError`] when the codec or the writer fails.
    pub fn add_frame(
        &mut self,
        ts_ns: u64,
        rgb: &[u8],
        native: &[i16],
        resampled: &[i16],
    ) -> Result<(), MkvError> {
        let encoded = ffv1::encode_frame(self.width, self.height, rgb)?;
        let mut cluster = Vec::new();
        ebml::uint_element(&mut cluster, 0xE7, ts_ns)?;
        ebml::element(
            &mut cluster,
            0xA3,
            &simple_block(TRACK_VIDEO, 0, true, &encoded),
        )?;
        if self.audio {
            let mut native_bytes = Vec::with_capacity(native.len().saturating_mul(2));
            for sample in native {
                native_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            ebml::element(
                &mut cluster,
                0xA3,
                &simple_block(TRACK_NATIVE, 0, true, &native_bytes),
            )?;
            let mut resampled_bytes = Vec::with_capacity(resampled.len().saturating_mul(2));
            for sample in resampled {
                resampled_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            ebml::element(
                &mut cluster,
                0xA3,
                &simple_block(TRACK_PRIMARY, 0, true, &resampled_bytes),
            )?;
        }
        ebml::element(&mut self.file, 0x1F43B675, &cluster)?;
        Ok(())
    }

    /// Flush and finish the file.
    ///
    /// # Errors
    ///
    /// Returns [`MkvError`] when the flush fails.
    pub fn finish(&mut self) -> Result<(), MkvError> {
        self.file.flush()?;
        Ok(())
    }
}

/// The EBML header payload with the Matroska doc type.
fn write_ebml_header(out: &mut Vec<u8>) -> Result<(), MkvError> {
    ebml::uint_element(out, 0x4286, 1)?; // EBMLVersion
    ebml::uint_element(out, 0x42F7, 1)?; // EBMLReadVersion
    ebml::uint_element(out, 0x42F2, 4)?; // EBMLMaxIDLength
    ebml::uint_element(out, 0x42F3, 8)?; // EBMLMaxSizeLength
    ebml::string_element(out, 0x4282, "matroska")?; // DocType
    ebml::uint_element(out, 0x4285, 4)?; // DocTypeVersion
    ebml::uint_element(out, 0x4286, 2)?; // DocTypeReadVersion
    Ok(())
}

/// One PCM `TrackEntry`: 16-bit little-endian mono at `rate`.
fn pcm_track_entry(track: u64, rate: u32, default_selected: bool) -> Result<Vec<u8>, MkvError> {
    let mut entry = Vec::new();
    ebml::uint_element(&mut entry, 0xD7, track)?;
    ebml::uint_element(&mut entry, 0x73C5, 0xDE10_0000 + track)?;
    ebml::uint_element(&mut entry, 0x83, 2)?;
    ebml::string_element(&mut entry, 0x86, CODEC_PCM)?;
    ebml::uint_element(&mut entry, 0x88, u64::from(u8::from(default_selected)))?;
    // The WAVEFORMATEX codec private: format tag 1 (PCM), one channel,
    // the rate, the byte rate, the 2-byte block align, 16-bit depth.
    let byte_rate = rate.saturating_mul(2);
    let mut wave = Vec::with_capacity(16);
    wave.extend_from_slice(&1u16.to_le_bytes()); // format tag: PCM
    wave.extend_from_slice(&1u16.to_le_bytes()); // channels
    wave.extend_from_slice(&rate.to_le_bytes()); // samples per second
    wave.extend_from_slice(&byte_rate.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes()); // block align
    wave.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    ebml::binary_element(&mut entry, 0x63A2, &wave)?;
    let mut audio = Vec::new();
    // The sampling frequency is an IEEE-754 double written by bit
    // pattern; no float arithmetic happens here.
    ebml::float_element(&mut audio, 0xB5, f64::from(rate).to_bits())?;
    ebml::uint_element(&mut audio, 0x9F, 1)?; // channels
    ebml::element(&mut entry, 0xE1, &audio)?;
    let mut out = Vec::new();
    ebml::element(&mut out, 0xAE, &entry)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(domain: &str, ticks: u64, device: &str, event: &str) -> ScriptEvent {
        ScriptEvent {
            domain: domain.to_string(),
            ticks,
            device: device.to_string(),
            event: event.to_string(),
            line: 1,
        }
    }

    #[test]
    fn event_payload_round_trips() {
        let event = event("main", 42, "pad", "press a");
        let bytes = encode_event(&event);
        let back = decode_event(&bytes).expect("decodes");
        assert_eq!(back.domain, "main");
        assert_eq!(back.ticks, 42);
        assert_eq!(back.device, "pad");
        assert_eq!(back.event, "press a");
        assert_eq!(back.line, 0);
    }

    #[test]
    fn short_payloads_are_rejected_without_panic() {
        let bytes = encode_event(&event("main", 42, "pad", "press a"));
        for cut in 0..bytes.len() {
            assert!(decode_event(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        // Trailing bytes are an error too.
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(decode_event(&padded).is_err());
    }

    #[test]
    fn input_form_round_trips_through_tags_attachment_and_blocks() {
        let raw_text = "[system]\nname = \"t\"\n";
        let config: SystemConfig = {
            let raw = demu_config::RawConfig::parse(
                "[system]\nname = \"t\"\ntriple = \"t\"\ndescription = \"d\"\n[clock.main]\nfrequency = \"1000\"\n[[device]]\nname = \"cpu0\"\nkind = \"smoke.cpu\"\ndomain = \"main\"\n[device.dividers]\ncpu = 1\n[[bus]]\nname = \"b\"\naddress_bits = 16\nowner = \"cpu0\"\n",
            )
            .expect("raw parses");
            demu_config::validate(&raw).expect("valid").0
        };
        let events = vec![
            event("main", 0, "cpu0", "nop"),
            event("main", 5, "cpu0", "nop"),
        ];
        let path = std::env::temp_dir().join("demu-mkv-test-input.mkv");
        write_input_file(&path, &config, raw_text, &events).expect("writes");
        let read = read_input_file(&path).expect("reads");
        std::fs::remove_file(&path).ok();
        assert_eq!(read.config_text.as_deref(), Some(raw_text));
        assert_eq!(read.triple.as_deref(), Some("t"));
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.events[0].domain, "main");
        assert_eq!(read.events[0].ticks, 0);
        assert_eq!(read.events[1].ticks, 5);
        assert_eq!(read.events[1].device, "cpu0");
    }

    #[test]
    fn clusters_keep_file_order_and_timestamps() {
        let path = std::env::temp_dir().join("demu-mkv-test-clusters.mkv");
        let config: SystemConfig = {
            let raw = demu_config::RawConfig::parse(
                "[system]\nname = \"t\"\ntriple = \"t\"\ndescription = \"d\"\n[clock.main]\nfrequency = \"1000\"\n[[device]]\nname = \"cpu0\"\nkind = \"smoke.cpu\"\ndomain = \"main\"\n[device.dividers]\ncpu = 1\n[[bus]]\nname = \"b\"\naddress_bits = 16\nowner = \"cpu0\"\n",
            )
            .expect("raw parses");
            demu_config::validate(&raw).expect("valid").0
        };
        let events = vec![event("main", 3, "cpu0", "a"), event("main", 1, "cpu0", "b")];
        write_input_file(&path, &config, "text", &events).expect("writes");
        let clusters = read_clusters(&path).expect("reads");
        std::fs::remove_file(&path).ok();
        assert_eq!(clusters.len(), 2);
        // File order: the first event's cluster first, in nanoseconds
        // of 1000 Hz ticks.
        assert_eq!(clusters[0].timestamp_ns, 3_000_000);
        assert_eq!(clusters[1].timestamp_ns, 1_000_000);
        assert_eq!(clusters[0].blocks.len(), 1);
        assert_eq!(clusters[0].blocks[0].0, TRACK_PRIMARY);
    }
}
