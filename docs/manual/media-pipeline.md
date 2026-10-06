# The demu media pipeline

This chapter explains the media pipeline of `demu`: the input script,
the replay scheduler, the Matroska forms, the video codec, and the
audio resampling. The technical design sections 12 and 26 specify the
same material on the design side. The manual records the user-facing
rules.

The pipeline is deterministic from end to end. The same machine, the
same script, and the same commands produce byte-identical files and
byte-identical final state on every run.

## 1. The input script

An input script is a text file. One event per line:

```
#<domain>:<ticks> <device> <event text>
```

The `#<domain>:<ticks>` stamp is the canonical time form. The domain
names a clock domain of the loaded configuration. The ticks are a tick
count in that domain: decimal, or `0x` hex. The device names a device
of the loaded configuration. The event text is one event in the
grammar of that device family.

A worked script for the Game Boy:

```
# boot demo starts at tick 0; wait 6 frames, press start
#main:402176 joypad press start
label walk
#main:404416 joypad press right
#main:404544 joypad release right
#main:406784 joypad press a
#main:407040 joypad release a
repeat walk 4
```

### 1.1 Stamps and comments

- `#<domain>:<ticks> <device> <event>` is one event. The domain must
  be a declared clock domain. The device must be a declared device.
- `#<ns> <device> <event>` is the nanosecond form. It converts
  against the machine's primary domain, the first clock of the
  name-sorted clock map, with the boundary rule: exact rational
  arithmetic and round-half-to-even.
- A `#` token that is not a stamp shape starts a comment. A space
  before a `#` after the timestamp position starts a comment.
- Blank lines are ignored.
- `label <name>` starts a named cyclic section. `repeat <name>
  <count>` copies the section events `count` times in total. Each
  copy shifts by the section span: the last event tick minus the
  first event tick of the section. So the first event of copy `k`
  lands exactly on the tick of the last event of copy `k-1`, and the
  copies fire in script order at the shared tick.

Label and repeat rules:

- A label must come before its repeat line.
- One section uses one stamp form and one domain. Mixed forms,
  mixed domains, and zero spans are errors.
- Label names are non-empty, without whitespace and without `#`.
- Duplicate labels, unknown labels, empty sections, and bad counts
  (zero or unparseable) are errors with a `line {n}` prefix.

### 1.2 Event text

demu validates the device and domain names at load time. The event
text itself is validated at injection time by the target device. A
script with an unknown event word loads and then stops the run at the
event's instant with a `rejected` error. Read the machine chapter for
the event words of each device.

### 1.3 Script errors

Every script error names the line: `line {n}: {message}`. The common
messages: `bad timestamp`, `the event line needs a device name`,
`bad device name`, `unknown clock domain`, `unknown device`,
`duplicate label`, `unknown label`, and ``expected a timestamp
event, `label`, or `repeat```.

## 2. The replay scheduler

`script <file>` installs the script as the run schedule. Every run
command then applies events at their exact instants:

- Pending events stay in script order.
- The scheduler compares each event instant against the primary
  domain tick by exact rational cross-multiplication in 128-bit
  integer arithmetic. No float math and no rounding drift.
- For an event on another domain, the primary clock stops no earlier
  than the event's instant computed exactly.
- When a finite run target sits past every queued event, the primary
  clock advances to the target. A free run still stops only at its
  hook condition. A machine that no longer advances reports a
  stalled run.

Two machines that replay the same script produce byte-identical
states. The Matroska input form carries the exact tick values, so a
replay from a Matroska file is exact, not a nanosecond round trip.

## 3. The Matroska forms

demu implements its own EBML primitives and two file forms: the input
form and the render form.

### 3.1 The input form

Written by `mkv-input`. One file, one input event track:

| Element | Value |
|---|---|
| Track 1, TrackType | `0x21` (the demu input metadata track type) |
| CodecID | `D_DEMU/INPUT/1` |
| Codec private | the 8-byte magic `DEMUINP1` |
| Blocks | one block per event in file order |
| Clusters | one cluster per event, at the event's nanosecond time |

The block payload is length-prefixed fields:
`u16le domain | u64le ticks | u16le device | u16le event text`. The
replay reads the exact tick values from the payload.

The system configuration rides on the file:

- The tag `demu.config` carries the configuration TOML text.
- The attachment `config.toml` (`application/toml`) carries the same
  bytes.
- The tags `demu.triple` and `demu.machine` carry the machine summary.

`mkv-render` reads the config from the file, rebuilds the machine
through the binary's machine factory, and never uses the session
machine. `render-session` records the current session machine.

### 3.2 The render form

Written by `mkv-render` and `render-session`. One cluster per
emulated display frame at TimestampScale `1`, the nanosecond
timebase. The track layout:

| Track | Content | Codec |
|---:|---|---|
| 1 | the resampled 48 kHz PCM track. First and default audio. | `A_PCM/INT/LIT`, WAVEFORMATEX private |
| 2 | the FFV1 lossless video, one frame per display frame | `V_FFV1`, demu's codec-configuration record |
| 3 | the native-rate PCM track | `A_PCM/INT/LIT`, WAVEFORMATEX private |

Cluster timestamps come from the primary domain tick at the vblank
of each frame. A machine without an audio device records video only.
A machine without a video device is an error. The render segment
opens with the EBML unknown-size marker and stays open to the end of
file, because the frame clusters stream as they record.

`mkv-render` reads the config from the file, rebuilds the machine
through the binary's machine factory, and never uses the session
machine. `render-session` records the current session machine.

### 3.3 The frame clock

One video block carries one emulated display frame. The frame count
follows the machine's declared frame clock: one vblank event per
display frame. A recorded file plays at the machine's real frame
rate, because the cluster timestamps derive from the machine ticks
through the boundary-conversion rule.

## 4. The video codec: FFV1

demu implements FFV1 lossless in `demu-mkv`:

- The demu build is a range-coder FFV1 with one slice per frame and a
  per-slice CRC.
- The run mode is not implemented. The codec encodes each plane with
  the range-coder context model only.
- The codec-configuration record rides as the codec private of the
  video track.
- Every video block decodes back to the exact emulated frame. Any
  demu build with the same version decodes the file.

Common-player playback is the known follow-up evidence pass. The
current build targets demu-to-demu round trips: `mkv-render`
output decodes to the emulated frames through demu itself.

## 5. Audio capture and resampling

- demu fully simulates every sound chip. The chips drive IRQs, DMA,
  and timing, so they run even with no capture active.
- There is no live audio output. demu is headless.
- Capture goes to the Matroska tracks only.
- The recorder drains every audio device after each completed display
  frame, in device declaration order, and concatenates the chunks
  into the native track. One sample rate must serve all audio
  devices of the machine.
- The 48 kHz track feeds the same chunks through the rational
  phase-accumulator resampler of `demu-resample`.

### 5.1 The resampler

`demu-resample` holds a rational phase accumulator keyed on the
pushed source samples:

- Output sample `i` takes the source sample at index
  `floor(i * source_rate / 48000)`.
- The comparison is incremental in 128-bit integer arithmetic.
- The output over any split of push calls is bit-identical. Any two
  runs produce the same stream.
- A machine whose audio and frame clocks differ in ratio accumulates
  that exact ratio drift in the native-track sample counts. The
  cluster timestamps from the machine ticks show the same per-frame
  spacing.

## 6. The pipeline commands in one flow

A full deterministic recording session:

```
$ demu --config rp2A03-nintendo-nes-ntsc demo.nes
demu> script input-demo.scr
loaded 18 input events
demu> run-frame 2
ran to tick 686092
demu> render-session 2 -o demo-session.mkv
demu> quit
bye
```

Then verify the round trip:

```
$ demu --json --config rp2A03-nintendo-nes-ntsc demo.nes <<'EOF'
{"id":1,"cmd":"mkv-render","args":["input-demo.mkv","2","-o","demo-render.mkv"]}
{"id":2,"cmd":"quit"}
EOF
```

The render builds the machine from the file's embedded config,
replays the events at their exact instants, and records the frames.
The same inputs and commands give byte-identical files.