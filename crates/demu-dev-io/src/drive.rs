//! The shared 5.25-inch drive core of the Apple disk controllers.
//!
//! The Disk II controller card (`a2.disk2`) and the IIgs IWM
//! (`iigs.iwm`) drive the same hardware through different register
//! maps. Both share this core: the two drive image slots, the
//! four-phase half-track arm, and the raw byte-stream media.

/// The bytes one drive image carries per track.
pub(crate) const TRACK_SPAN: usize = 256;

/// The track count of the 5.25-inch form.
pub(crate) const TRACKS: usize = 35;

/// The half-track count of the arm: 35 tracks at half steps.
pub(crate) const HALF_TRACKS: u8 = 69;

/// One drive of the controller: the arm state and the media.
#[derive(Debug)]
pub(crate) struct Drive {
    /// The arm position in half tracks, 0 through 68.
    pub(crate) half_track: u8,
    /// The head byte position inside the track.
    pub(crate) head: usize,
    /// The write-protect tab of the inserted disk.
    pub(crate) protected: bool,
    /// The raw media image, `TRACKS` by `TRACK_SPAN` when present.
    pub(crate) image: Option<Vec<u8>>,
    /// The data latch of the last read or write.
    pub(crate) latch: u8,
}

impl Drive {
    pub(crate) fn new() -> Self {
        Self {
            half_track: 0,
            head: 0,
            protected: false,
            image: None,
            latch: 0,
        }
    }

    /// The head byte slot on the selected track.
    pub(crate) fn slot(&self) -> usize {
        (usize::from(self.half_track / 2) * TRACK_SPAN + self.head % TRACK_SPAN)
            .min(TRACKS * TRACK_SPAN - 1)
    }
}

/// Drives one stepper phase: a pulse of the phase next in the phase
/// cycle advances the arm one half track, a pulse of the phase before
/// the last pulsed one rewinds it, and a repeated pulse of one phase
/// does nothing.
pub(crate) fn pulse_phase(drive: &mut Drive, phase: usize, last_phase: &mut usize) {
    let delta: i16 = if phase == (*last_phase + 1) & 0x03 {
        1
    } else if phase == (*last_phase + 3) & 0x03 {
        -1
    } else {
        0
    };
    *last_phase = phase;
    if delta == 0 {
        return;
    }
    let moved = i16::from(drive.half_track) + delta;
    drive.half_track = moved.clamp(0, i16::from(HALF_TRACKS)) as u8;
    drive.head = usize::from(drive.half_track / 2) * TRACK_SPAN;
}

/// Load a boot-state image into the drive: the drive-one media, trimmed to
/// the standard media size.
pub(crate) fn load_boot_image(drive: &mut Drive, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    drive.image = Some(bytes[..bytes.len().min(TRACKS * TRACK_SPAN)].to_vec());
}
