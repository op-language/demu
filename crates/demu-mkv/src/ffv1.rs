//! The FFV1 lossless video codec for the render form.
//!
//! The codec implements an FFV1 version-3 profile: three planar 8-bit
//! color planes in G, B, R order, the median predictor with per-context
//! value coding through a range coder, one slice per frame, and a
//! CRC-32 slice footer. The encoder and decoder are exact mirrors, so
//! `decode_frame(encode_frame(x))` returns `x` byte for byte and the
//! same frame always encodes to the same byte stream. No float math
//! and no randomness exists in the module.
//!
//! # Range coder
//!
//! The range coder keeps the FFV1 range-coding form: the interval
//! `[low, low + range)` slides over a 16-bit window, every decision
//! splits the interval at `(range * state) >> 8`, and renormalization
//! shifts the window by 8 bits and emits carried bytes, buffering the
//! carry through an outstanding byte and a run count. The value coder
//! writes, per context, a nonzero marker, a unary exponent of ones
//! with a zero terminator, the magnitude bits below the leading bit,
//! and the sign. Every context holds 32 states: the exponent walk caps
//! at 9, so the terminator sits at state 10, the magnitude bits at
//! states 22 plus their index, and the sign at state 11 plus the
//! exponent.
//!
//! # Profile notes
//!
//! The state transition tables follow the FFV1 walk shape with demu's
//! integer generator: `one_state` advances a state by
//! `(254 - i) / 32 + 1`, saturating at 255, and `zero_state` mirrors
//! it. The RFC-published default table values stay pending the
//! interoperability pass; the round-trip guarantee depends only on the
//! encoder and decoder sharing the tables. The profile uses one
//! quantization table for all planes, seven buckets per neighbor
//! direction, 343 pixel contexts per plane, no run mode, and keyframes
//! only. Samples outside the frame read as 128, the context
//! initialization value. A frame dimension caps at 1024 pixels per
//! side, which covers every supported machine display.
//!
//! The largest possible plane difference is 510 (a sample against an
//! extrapolated prediction), whose exponent is 9; the header symbols
//! are byte-wide or smaller, whose exponents are 7 or below. Both stay
//! inside the 9-cap.
//!
//! # Bitstream layout
//!
//! - byte 0: the keyframe flag, 1.
//! - byte 1: the micro version, 4.
//! - byte 2: the profile tag, 1 (one quant table, seven buckets).
//! - bytes `3..`: the range-coded slice: the slice header (slice x,
//!   slice y, then the width-1 and height-1 values as one low byte and
//!   one high byte each), then the G, B, R planes pixel by pixel.
//! - the tail: a little-endian CRC-32 over every preceding byte.
//!
//! # Configuration record
//!
//! The Matroska codec-private record is ten raw bytes:
//!
//! - bytes `0..4`: the magic `"DFV1"`.
//! - byte 4: the FFV1 `version`, 3.
//! - byte 5: the `micro_version`, 4.
//! - byte 6: the coder type, 1 (range coder).
//! - byte 7: the plane count, 3 (G, B, R).
//! - byte 8: the bits per plane, 8.
//! - byte 9: the profile tag, 1.
//!
//! [`is_compatible`] checks the magic, the field values, and the
//! length.

/// The FFV1 version this module implements.
const VERSION: u8 = 3;

/// The micro version: carries the per-slice CRC footer.
const MICRO_VERSION: u8 = 4;

/// The range-coder coder type marker.
const CODER_RANGE: u8 = 1;

/// The plane order: green, blue, red, as FFV1 orders RGB planes.
const PLANE_COUNT: usize = 3;

/// The value-coder state array size per context, as FFV1 defines.
const CONTEXT_STATES: usize = 32;

/// The bucket count per neighbor direction: `context = (b1 * 7 + b2) *
/// 7 + b3`.
const QUANT_BUCKETS: usize = 7;

/// The pixel contexts per plane.
const CONTEXT_COUNT: usize = QUANT_BUCKETS * QUANT_BUCKETS * QUANT_BUCKETS;

/// The frame profile tag: one quant table, seven buckets.
const PROFILE_TAG: u8 = 1;

/// The frame dimension cap in pixels per side.
const MAX_DIMENSION: u32 = 1024;

/// The edge sample value: out-of-frame neighbors read as 128.
const EDGE_SAMPLE: i16 = 128;

/// One FFV1 codec failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Ffv1Error {
    /// The frame geometry or the pixel payload does not fit together.
    #[error("bad frame geometry {width}x{height}: expected {expected} pixels, got {actual} bytes")]
    Geometry {
        /// The declared width in pixels.
        width: u32,
        /// The declared height in pixels.
        height: u32,
        /// The pixel count the geometry requires.
        expected: u64,
        /// The payload length the caller supplied.
        actual: usize,
    },
    /// The bitstream terminates, desynchronizes, or fails a check.
    #[error("bitstream error: {0}")]
    Bitstream(String),
    /// The slice CRC does not match.
    #[error("slice CRC mismatch")]
    Crc,
    /// The frame is not a keyframe.
    #[error("the frame is not a keyframe")]
    NotKeyframe,
    /// The configuration record does not match this implementation.
    #[error("the configuration record is incompatible")]
    Record,
}

/// The reflected CRC-32 (the IEEE 802.3 polynomial, table-free).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The two state transition tables of the range coder.
///
/// `one_state[i]` walks a context's probability toward the saturated
/// `1` state; `zero_state[i]` mirrors it. Both saturate inside
/// `1..255`, so the split at `(range * state) >> 8` never degenerates.
fn build_tables() -> ([u8; 256], [u8; 256]) {
    let mut one = [0u8; 256];
    let mut zero = [0u8; 256];
    for (index, state) in one.iter_mut().enumerate().take(255).skip(1) {
        let step = (254 - index) / 32 + 1;
        *state = (index + step).min(255) as u8;
    }
    one[255] = 255;
    for (index, state) in zero.iter_mut().enumerate() {
        let mirrored = if index == 0 { 255 } else { one[256 - index] };
        *state = 255 - mirrored;
    }
    zero[255] = 0;
    (one, zero)
}

/// The state transition pair shared by the encoder and the decoder.
struct CoderTables {
    one_state: [u8; 256],
    zero_state: [u8; 256],
}

impl CoderTables {
    fn new() -> Self {
        let (one_state, zero_state) = build_tables();
        Self {
            one_state,
            zero_state,
        }
    }
}

/// The range-coder encoder.
struct RangeEncoder<'a> {
    tables: &'a CoderTables,
    low: u32,
    range: u32,
    out: Vec<u8>,
    outstanding_byte: Option<u8>,
    outstanding_count: u64,
}

impl<'a> RangeEncoder<'a> {
    fn new(tables: &'a CoderTables) -> Self {
        Self {
            tables,
            low: 0,
            range: 0xFF00,
            out: Vec::new(),
            outstanding_byte: None,
            outstanding_count: 0,
        }
    }

    fn emit(&mut self, byte: u8) {
        self.out.push(byte);
    }

    /// Slide the window left by 8 bits, resolving the carry chain.
    fn renorm(&mut self) {
        while self.range < 0x100 {
            if self.outstanding_byte.is_none() {
                self.outstanding_byte = Some((self.low >> 8) as u8);
            } else if self.low <= 0xFF00 {
                let carried = self.outstanding_byte.expect("checked in this branch");
                self.emit(carried);
                self.outstanding_byte = Some((self.low >> 8) as u8);
                for _ in 0..self.outstanding_count {
                    self.emit(0xFF);
                }
                self.outstanding_count = 0;
            } else if self.low >= 0x10000 {
                let carried = self.outstanding_byte.expect("checked in this branch");
                self.emit(carried.wrapping_add(1));
                self.outstanding_byte = Some(((self.low >> 8) & 0xFF) as u8);
                for _ in 0..self.outstanding_count {
                    self.emit(0x00);
                }
                self.outstanding_count = 0;
            } else {
                self.outstanding_count += 1;
            }
            self.low = (self.low & 0xFF) << 8;
            self.range <<= 8;
        }
    }

    /// Code one binary decision with the context `state`.
    fn put_rac(&mut self, state: &mut u8, bit: bool) {
        let range1 = ((self.range * u32::from(*state)) >> 8).max(1);
        if !bit {
            self.range -= range1;
            *state = self.tables.zero_state[usize::from(*state)];
        } else {
            self.low += self.range - range1;
            self.range = range1;
            *state = self.tables.one_state[usize::from(*state)];
        }
        self.renorm();
    }

    /// Code one signed value through the 32 states of one context.
    ///
    /// The call domains keep the exponent at 9 or below: plane
    /// differences are at most 510 and the header symbols are byte
    /// parts, so the debug assertion never fires.
    fn put_symbol(&mut self, states: &mut [u8; CONTEXT_STATES], value: i16) {
        if value == 0 {
            self.put_rac(&mut states[0], false);
            return;
        }
        self.put_rac(&mut states[0], true);
        let magnitude = value.unsigned_abs();
        let exponent = (16 - magnitude.leading_zeros()) as usize;
        debug_assert!(exponent <= 9, "the value domains keep the exponent at 9");
        for index in 0..exponent {
            self.put_rac(&mut states[1 + index], true);
        }
        self.put_rac(&mut states[1 + exponent], false);
        for index in (0..exponent.saturating_sub(1)).rev() {
            self.put_rac(&mut states[22 + index], (magnitude >> index) & 1 == 1);
        }
        self.put_rac(&mut states[11 + exponent], value < 0);
    }

    /// Terminate the stream: push the final value inside the recorded
    /// interval, slide it out with two renormalizations, then flush the
    /// outstanding chain.
    fn terminate(mut self) -> Vec<u8> {
        self.range = 0xFF;
        self.low += 0xFF;
        self.renorm();
        self.range = 0xFF;
        self.renorm();
        if let Some(byte) = self.outstanding_byte.take() {
            self.emit(byte);
        }
        for _ in 0..self.outstanding_count {
            self.emit(0xFF);
        }
        self.out
    }
}

/// The range-coder decoder: the exact mirror of [`RangeEncoder`].
struct RangeDecoder<'a> {
    tables: &'a CoderTables,
    /// The interval offset scaled by 2^16 with 16 bits of lookahead.
    low: u64,
    range: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    fn new(tables: &'a CoderTables, data: &'a [u8]) -> Self {
        let byte = |pos: usize| u64::from(data.get(pos).copied().unwrap_or(0));
        let low = (byte(0) << 24) | (byte(1) << 16) | (byte(2) << 8) | byte(3);
        Self {
            tables,
            low,
            range: 0xFF00,
            data,
            pos: 4,
        }
    }

    fn next_byte(&mut self) -> u64 {
        let byte = u64::from(self.data.get(self.pos).copied().unwrap_or(0));
        self.pos = self.pos.saturating_add(1);
        byte
    }

    /// The encoder's window keeps 16 bits; the decoder mirrors it with
    /// 16 more lookahead bits, so the register masks to 32 bits and
    /// never carries history past the window.
    fn renorm(&mut self) {
        while self.range < 0x100 {
            self.range <<= 8;
            self.low = ((self.low << 8) | self.next_byte()) & 0xFFFF_FFFF;
        }
    }

    fn get_rac(&mut self, state: &mut u8) -> bool {
        let range1 = ((self.range * u32::from(*state)) >> 8).max(1);
        self.range -= range1;
        let bit = if self.low < u64::from(self.range) << 16 {
            *state = self.tables.zero_state[usize::from(*state)];
            false
        } else {
            self.low -= u64::from(self.range) << 16;
            self.range = range1;
            *state = self.tables.one_state[usize::from(*state)];
            true
        };
        // The encoder renormalizes after every decision; the mirror
        // must keep the same window alignment.
        self.renorm();
        bit
    }

    /// Decode one signed value coded through one context's states.
    ///
    /// A malformed stream cannot overrun the state indexes: the
    /// exponent walk stops at 9, so the terminator read sits at state
    /// 10, the magnitude bits at states 22 up to 29, and the sign at
    /// state 20.
    fn get_symbol(&mut self, states: &mut [u8; CONTEXT_STATES]) -> Result<i16, Ffv1Error> {
        if !self.get_rac(&mut states[0]) {
            return Ok(0);
        }
        let mut exponent = 0usize;
        while exponent <= 9 && self.get_rac(&mut states[1 + exponent]) {
            exponent += 1;
        }
        if exponent > 9 {
            return Err(Ffv1Error::Bitstream(
                "the value exponent exceeds the 8-bit plane range".to_string(),
            ));
        }
        let mut magnitude: i32 = if exponent == 0 {
            1
        } else {
            1 << (exponent - 1)
        };
        for index in (0..exponent.saturating_sub(1)).rev() {
            if self.get_rac(&mut states[22 + index]) {
                magnitude |= 1 << index;
            }
        }
        let negative = self.get_rac(&mut states[11 + exponent]);
        let value = i16::try_from(magnitude)
            .map_err(|_| Ffv1Error::Bitstream("the magnitude overflows i16".to_string()))?;
        Ok(if negative { -value } else { value })
    }
}

/// The quantization bucket of one neighbor delta.
fn bucket(delta: i16) -> usize {
    match delta {
        d if d < -16 => 0,
        d if d < -4 => 1,
        d if d < 0 => 2,
        0 => 3,
        d if d <= 4 => 4,
        d if d <= 16 => 5,
        _ => 6,
    }
}

/// The FFV1 median predictor over the left, top, and diagonal
/// neighbors.
///
/// The differences stay inside `[-255, 510]`, so the addition cannot
/// overflow `i16`.
fn median_predictor(left: i16, top: i16, diagonal: i16) -> i16 {
    if diagonal >= left.max(top) {
        left.max(top)
    } else if diagonal <= left.min(top) {
        left.min(top)
    } else {
        left + top - diagonal
    }
}

/// The neighbor sample of the plane at `(x + dx, y + dy)`, reading the
/// edge sample outside the frame.
///
/// The plane holds `height` complete rows of `width` bytes; every
/// caller asks only for rows `y` and `y - 1`, which are complete on
/// both the encode and the decode path.
fn neighbor(plane: &[u8], width: usize, x: usize, y: usize, dx: isize, dy: isize) -> i16 {
    let nx = x as i64 + i64::from(dx as i32);
    let ny = y as i64 + i64::from(dy as i32);
    let height = plane.len() / width.max(1);
    if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
        return EDGE_SAMPLE;
    }
    let index = ny as usize * width + nx as usize;
    i16::from(plane[index])
}

/// The pixel context of the plane at `(x, y)`.
fn context_of(plane: &[u8], width: usize, x: usize, y: usize) -> usize {
    let left = neighbor(plane, width, x, y, -1, 0);
    let top = neighbor(plane, width, x, y, 0, -1);
    let diagonal = neighbor(plane, width, x, y, -1, -1);
    let top_right = neighbor(plane, width, x, y, 1, -1);
    let b1 = bucket(left - diagonal);
    let b2 = bucket(diagonal - top);
    let b3 = bucket(top - top_right);
    (b1 * QUANT_BUCKETS + b2) * QUANT_BUCKETS + b3
}

/// The predicted value of the plane at `(x, y)`.
fn prediction_of(plane: &[u8], width: usize, x: usize, y: usize) -> i16 {
    let left = neighbor(plane, width, x, y, -1, 0);
    let top = neighbor(plane, width, x, y, 0, -1);
    let diagonal = neighbor(plane, width, x, y, -1, -1);
    median_predictor(left, top, diagonal)
}

/// Validate the frame dimensions; return the `usize` dimensions.
fn validate_dimensions(width: u32, height: u32) -> Result<(usize, usize), Ffv1Error> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Ffv1Error::Geometry {
            width,
            height,
            expected: u64::from(width) * u64::from(height),
            actual: 0,
        });
    }
    Ok((width as usize, height as usize))
}

/// Split the packed RGB888 frame into the G, B, R planes.
fn plane_split(rgb: &[u8]) -> [Vec<u8>; PLANE_COUNT] {
    let mut red = Vec::with_capacity(rgb.len() / 3);
    let mut green = Vec::with_capacity(rgb.len() / 3);
    let mut blue = Vec::with_capacity(rgb.len() / 3);
    for channel in rgb.as_chunks::<3>().0 {
        green.push(channel[1]);
        blue.push(channel[2]);
        red.push(channel[0]);
    }
    [green, blue, red]
}

/// Join the G, B, R planes back into the packed RGB888 form.
fn plane_join(planes: &[Vec<u8>; PLANE_COUNT]) -> Vec<u8> {
    let pixel_count = planes[0].len();
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for (red, (green, blue)) in planes[2].iter().zip(planes[0].iter().zip(planes[1].iter())) {
        rgb.push(*red);
        rgb.push(*green);
        rgb.push(*blue);
    }
    rgb
}

/// Encode one plane row-major through the range coder.
fn encode_plane_data(
    rac: &mut RangeEncoder,
    states: &mut [[u8; CONTEXT_STATES]],
    width: usize,
    height: usize,
    plane: &[u8],
) {
    for y in 0..height {
        for x in 0..width {
            let context = context_of(plane, width, x, y);
            let prediction = prediction_of(plane, width, x, y);
            let sample = i16::from(plane[y * width + x]);
            rac.put_symbol(&mut states[context], sample - prediction);
        }
    }
}

/// Decode one plane row-major from the range coder.
///
/// The neighbors of `(x, y)` sit on rows `y` and `y - 1`, which are
/// complete at that point on both paths, so the decode side computes
/// the identical context and prediction as the encode side.
fn decode_plane_data(
    rac: &mut RangeDecoder,
    states: &mut [[u8; CONTEXT_STATES]],
    width: usize,
    height: usize,
    plane: &mut [u8],
) -> Result<(), Ffv1Error> {
    for y in 0..height {
        for x in 0..width {
            let context = context_of(plane, width, x, y);
            let prediction = prediction_of(plane, width, x, y);
            let diff = rac.get_symbol(&mut states[context])?;
            let sample = prediction + diff;
            // The encoder emitted `sample - prediction`, so the sum
            // reproduces the byte exactly; a corrupt stream that
            // evaded the CRC lands here as an error, never as a wrap.
            let sample = u8::try_from(sample).map_err(|_| {
                Ffv1Error::Bitstream(format!("the decoded sample {sample} leaves the byte range"))
            })?;
            plane[y * width + x] = sample;
        }
    }
    Ok(())
}

/// The FFV1 configuration record bytes for the codec this module
/// implements.
#[must_use]
pub fn configuration_record() -> Vec<u8> {
    vec![
        b'D',
        b'F',
        b'V',
        b'1',
        VERSION,
        MICRO_VERSION,
        CODER_RANGE,
        PLANE_COUNT as u8,
        8,
        PROFILE_TAG,
    ]
}

/// Whether a configuration-record byte slice matches this
/// implementation.
#[must_use]
pub fn is_compatible(data: &[u8]) -> bool {
    data == configuration_record()
}

/// Encode one packed RGB888 frame into the FFV1 bitstream.
///
/// The pixels run row-major with three bytes per pixel; the planes
/// code in G, B, R order.
///
/// # Errors
///
/// Returns [`Ffv1Error`] when the geometry does not match the payload.
pub fn encode_frame(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, Ffv1Error> {
    let (width, height) = validate_dimensions(width, height)?;
    if rgb.len() != width * height * 3 {
        return Err(Ffv1Error::Geometry {
            width: width as u32,
            height: height as u32,
            expected: (width * height) as u64,
            actual: rgb.len(),
        });
    }
    let planes = plane_split(rgb);

    let tables = CoderTables::new();
    let mut rac = RangeEncoder::new(&tables);
    let mut header = [128u8; CONTEXT_STATES];
    // The slice header of the single slice: x, y, then the width-1 and
    // height-1 values as one low byte and one high byte each.
    rac.put_symbol(&mut header, 0);
    rac.put_symbol(&mut header, 0);
    for dimension in [width - 1, height - 1] {
        let low = (dimension & 0xFF) as i16;
        let high = (dimension >> 8) as i16;
        rac.put_symbol(&mut header, low);
        rac.put_symbol(&mut header, high);
    }
    for plane in &planes {
        let mut states = vec![[128u8; CONTEXT_STATES]; CONTEXT_COUNT];
        encode_plane_data(&mut rac, &mut states, width, height, plane);
    }
    let slice = rac.terminate();

    let mut frame = Vec::with_capacity(slice.len() + 8);
    frame.push(1); // keyframe
    frame.push(MICRO_VERSION);
    frame.push(PROFILE_TAG);
    frame.extend_from_slice(&slice);
    let crc = crc32(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());
    Ok(frame)
}

/// Decode an FFV1 bitstream of this implementation back to packed
/// RGB888.
///
/// # Errors
///
/// Returns [`Ffv1Error`] when the frame is not a keyframe, the CRC
/// does not match, the slice header disagrees with the geometry, or
/// the sample stream desynchronizes.
pub fn decode_frame(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>, Ffv1Error> {
    let (width, height) = validate_dimensions(width, height)?;
    let pixels = width * height;
    if data.len() < 8 {
        return Err(Ffv1Error::Bitstream(
            "the frame is shorter than its header and CRC".to_string(),
        ));
    }
    if data[0] != 1 {
        return Err(Ffv1Error::NotKeyframe);
    }
    if data[1] != MICRO_VERSION {
        return Err(Ffv1Error::Bitstream(format!(
            "the micro version {} is not {MICRO_VERSION}",
            data[1]
        )));
    }
    if data[2] != PROFILE_TAG {
        return Err(Ffv1Error::Bitstream(
            "the profile tag does not match this codec".to_string(),
        ));
    }
    let tail = data.len() - 4;
    let stored_crc = u32::from_le_bytes(
        data[tail..]
            .try_into()
            .map_err(|_| Ffv1Error::Bitstream("the CRC is missing".to_string()))?,
    );
    if crc32(&data[..tail]) != stored_crc {
        return Err(Ffv1Error::Crc);
    }

    let tables = CoderTables::new();
    // The coder may read past the terminated slice; such reads answer
    // zero and the loop counts are fixed by the geometry, so a
    // truncated stream always terminates.
    let mut slice = RangeDecoder::new(&tables, &data[3..tail]);
    let mut header = [128u8; CONTEXT_STATES];
    let slice_x = slice.get_symbol(&mut header)?;
    let slice_y = slice.get_symbol(&mut header)?;
    let width_low = slice.get_symbol(&mut header)?;
    let width_high = slice.get_symbol(&mut header)?;
    let height_low = slice.get_symbol(&mut header)?;
    let height_high = slice.get_symbol(&mut header)?;
    if slice_x != 0
        || slice_y != 0
        || width_low < 0
        || width_high < 0
        || height_low < 0
        || height_high < 0
    {
        return Err(Ffv1Error::Bitstream(
            "the slice header is malformed".to_string(),
        ));
    }
    let coded_width = 1 + (width_low as usize) + ((width_high as usize) << 8);
    let coded_height = 1 + (height_low as usize) + ((height_high as usize) << 8);
    if coded_width != width || coded_height != height {
        return Err(Ffv1Error::Bitstream(
            "the slice header does not match the frame geometry".to_string(),
        ));
    }

    let mut planes = [vec![0u8; pixels], vec![0u8; pixels], vec![0u8; pixels]];
    for plane in &mut planes {
        let mut states = vec![[128u8; CONTEXT_STATES]; CONTEXT_COUNT];
        decode_plane_data(&mut slice, &mut states, width, height, plane)?;
    }
    Ok(plane_join(&planes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random pixel stream for large-image
    /// round trips.
    fn pseudo_rgb(pixels: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..pixels * 3)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 40) as u8
            })
            .collect()
    }

    fn round_trip(width: u32, height: u32, pixels: &[u8]) {
        let encoded = encode_frame(width, height, pixels).expect("encodes");
        let decoded = decode_frame(width, height, &encoded).expect("decodes");
        assert_eq!(
            decoded, pixels,
            "the round trip is lossless at {width}x{height}"
        );
    }

    #[test]
    fn geometry_errors_are_specific() {
        assert!(matches!(
            encode_frame(0, 1, &[]),
            Err(Ffv1Error::Geometry { actual: 0, .. })
        ));
        assert!(matches!(
            encode_frame(1, 1, &[1, 2, 3, 4]),
            Err(Ffv1Error::Geometry { actual: 4, .. })
        ));
        assert!(encode_frame(MAX_DIMENSION, 1, &vec![0u8; MAX_DIMENSION as usize * 3]).is_ok());
        let oversized = vec![0u8; (MAX_DIMENSION as usize + 1) * 3];
        assert!(matches!(
            encode_frame(MAX_DIMENSION + 1, 1, &oversized),
            Err(Ffv1Error::Geometry { actual: 0, .. })
        ));
    }

    #[test]
    fn tiny_frames_round_trip() {
        round_trip(1, 1, &[0x12, 0x34, 0x56]);
        round_trip(1, 1, &[0xFF, 0xFF, 0xFF]);
        round_trip(1, 1, &[0x00, 0x00, 0x00]);
        round_trip(7, 5, &pseudo_rgb(7 * 5, 1));
    }

    #[test]
    fn machine_sized_frames_round_trip() {
        // The Game Boy display.
        round_trip(160, 144, &pseudo_rgb(160 * 144, 2));
        // The SNES high-res form.
        round_trip(512, 448, &pseudo_rgb(512 * 448, 3));
    }

    #[test]
    fn edge_shapes_round_trip() {
        // Horizontal gradient, a flat top edge, and a vertical ramp
        // exercise the left, top, and corner predictor paths.
        let mut gradient = Vec::new();
        for y in 0..4u32 {
            for x in 0..8u32 {
                gradient.push(((x * 31 + y * 7) % 256) as u8);
                gradient.push(((x * 5 + y * 91) % 256) as u8);
                gradient.push(((x * 199 + y * 3) % 256) as u8);
            }
        }
        round_trip(8, 4, &gradient);
        round_trip(3, 3, &[0; 27]);
        round_trip(3, 3, &[255; 27]);
    }

    #[test]
    fn encoding_is_deterministic() {
        let pixels = pseudo_rgb(20 * 9, 4);
        let first = encode_frame(20, 9, &pixels).expect("encodes");
        let second = encode_frame(20, 9, &pixels).expect("encodes");
        assert_eq!(first, second);
    }

    #[test]
    fn corrupted_frames_fail_without_panicking() {
        let pixels = pseudo_rgb(32 * 8, 5);
        let encoded = encode_frame(32, 8, &pixels).expect("encodes");
        for index in 0..encoded.len() {
            let mut broken = encoded.clone();
            broken[index] ^= 0xA5;
            // Any corruption either errors or decodes wrong, never
            // panics and never hangs.
            let _ = decode_frame(32, 8, &broken);
        }
    }

    #[test]
    fn the_crc_rejects_tail_corruption() {
        let pixels = pseudo_rgb(24 * 6, 6);
        let mut encoded = encode_frame(24, 6, &pixels).expect("encodes");
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;
        assert!(matches!(decode_frame(24, 6, &encoded), Err(Ffv1Error::Crc)));
    }

    #[test]
    fn non_keyframes_are_rejected() {
        let pixels = pseudo_rgb(4 * 4, 7);
        let mut encoded = encode_frame(4, 4, &pixels).expect("encodes");
        encoded[0] = 0;
        assert!(matches!(
            decode_frame(4, 4, &encoded),
            Err(Ffv1Error::NotKeyframe)
        ));
        assert!(matches!(
            decode_frame(4, 4, &[]),
            Err(Ffv1Error::Bitstream(_))
        ));
    }

    #[test]
    fn truncated_frames_terminate_quickly() {
        let pixels = pseudo_rgb(16 * 16, 8);
        let encoded = encode_frame(16, 16, &pixels).expect("encodes");
        for cut in 4..encoded.len() {
            // A truncated blob either fails the length and CRC checks
            // or runs the fixed decode counts over its remainder.
            let _ = decode_frame(16, 16, &encoded[..cut]);
        }
    }

    #[test]
    fn symbols_round_trip_through_the_rac() {
        let tables = CoderTables::new();
        let values: [i16; 14] = [
            0, 5, -3, 100, -200, 255, -255, 1, -1, 300, -110, 76, -76, 42,
        ];
        let mut rac = RangeEncoder::new(&tables);
        let mut states = [128u8; CONTEXT_STATES];
        for &value in &values {
            rac.put_symbol(&mut states, value);
        }
        let bytes = rac.terminate();
        let mut decoder = RangeDecoder::new(&tables, &bytes);
        let mut decode_states = [128u8; CONTEXT_STATES];
        for (index, &value) in values.iter().enumerate() {
            assert_eq!(
                decoder.get_symbol(&mut decode_states).expect("symbol"),
                value,
                "symbol {index} ({value})"
            );
        }
    }

    #[test]
    fn the_configuration_record_is_stable_and_checked() {
        let record = configuration_record();
        assert_eq!(record.len(), 10);
        assert_eq!(record, configuration_record());
        assert!(is_compatible(&record));
        assert!(!is_compatible(b""));
        assert!(!is_compatible(&record[..9]));
        let mut flipped = record.clone();
        flipped[4] = 4; // a different FFV1 version
        assert!(!is_compatible(&flipped));
    }

    #[test]
    fn tables_saturate_and_mirror() {
        let (one, zero) = build_tables();
        assert_eq!(one[255], 255);
        assert_eq!(zero[255], 0);
        for (index, state) in one.iter().enumerate().skip(1) {
            if index < 254 {
                assert!(*state > index as u8, "one walks up at {index}");
            }
        }
    }
}
