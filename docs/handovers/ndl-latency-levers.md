# NDL latency: PTS lead trim, slice-progressive feed, PCM audio plane as the default route

**Original request:** review how streamed video and audio are fed into the NDL pipeline and find
latency improvements, researching NDL and ss4s implementation details. Then implement all of it;
then make the PCM plane the default and delete what the SDL path no longer needs, offering only the
channel layouts the TV can actually output.
**Branch:** `ndl-latency-levers` - **Status:** ready for on-device verification

## Scope
Three latency levers, then a follow-up pass that promoted two of them to default and reduced the
SDL audio path to an internal fallback. Out of scope (user deferred): the `lock_ffi` contention
between the video feed and the audio plane; arming any client-side A/V correction loop (the
estimator is gone, see below).

## Measured so far (two CX sessions, 5.1 and 7.1)
- `pts lead: trimming 35.7ms` / `38.8ms total` — reproducible, and the only lever with numbers.
- Both runs predate the promotion, so they ran `software Opus decode -> SDL2` with no frame parts:
  levers 2 and 3 have **never executed on hardware**.
- What is proven is that NDL's stamps stopped sitting 35-39 ms in the future. On-glass improvement
  follows from `pauseAtDecodeTime` but is not measured — nothing in the app can see presentation.

## Done
- **PTS lead trim** (`session/timeline.rs`): `HostPtsAnchor` measures its own lead, subtracts the
  minimum over 500 ms windows, ramped at ¼ frame interval per frame, inside `TRIM_SETTLE_NS`
  (600 ms). `sink::pts_trimmed_ms` reaches the video heartbeat. Five in-tree tests.
- **`ready_for_audio()`**: `sink::submit` holds `latch_pts_offset` until the trim settles, so the
  audio plane never anchors to a timeline still being pulled earlier. Replaced the earlier
  `SinkConfig::trim_pts_lead` route gate — the trim is now unconditional.
- **Slice-progressive feed** (`session/pump.rs` `AuParts`/`PartStep`, `FrameFlags::partial`): on for
  every NDL v2 session, no setting. Enforces core's part contract, reports a break as loss (reusing
  freeze-until-reanchor), and the sink skips per-frame reference points on a non-final piece.
- **PCM plane is the default audio route** (`AudioRoute::{Software,NdlOpus,NdlPcm}` in
  `session/connect.rs`): `ffi::AudioPcmInfo` byte-exact with `webos-userland`, `NdlAudioConfig` enum
  with format-aware `silence()` (prime + clock plane), `PcmFeed` in `platform/webos/audio.rs`,
  `pump::ndl_pcm_audio_pump` stamping concealment at `pts - lead`.
- **SDL path reduced to a fallback**: deleted `JitterPolicy`/`WEBOS_TUNING`, the crossfaded shed,
  `AvSync` and its `SyncCells`, `in_flight` accounting. What remains is a prime-25 ms-then-serve
  ring, used only when the load produced no audio plane. Overlay still shows `Opus SW … buf N ms`;
  the NDL routes show `PCM HW` / `Opus HW`.
- **Layouts follow the TV, not the decoder**: `ffi::multichannel_pcm()` calls
  `NDL_DirectAudioSupportMultiChannel` (optional symbol; absent ⇒ stereo), surfaced as
  `ndl::audio_plane_max_channels()` → 6 or 2, feeding `core::caps` and therefore the Settings
  dropdown. 7.1 is never offered; the 7.1 *decode* path survives (`PcmFeed` folds 8→6 at −3 dB).
- **Row locks**: `RowLock::StereoOnly` recaptioned "Your TV's audio output only carries stereo";
  new `RowLock::OffloadStereoOnly` when Opus offload is on, plus a `Settings::clamp` rule forcing
  `audio_channels = 2` in that case.
- Experimental now holds two rows (Game mode, Audio processing). The `ndl_audio_pcm` and
  `ndl_frame_parts` settings were added and then removed again; neither exists.
- `docs/NOTES.md`: rewrote § "Audio", § "A/V sync", § "NDL's audio plane"'s route list, and added
  § "Latency levers on the NDL path".
- `task docker:check` + `docker:lint` clean, `task fmt` applied.

## Left
1. Run a stereo session. Expect `audio path: software Opus decode -> NDL PCM plane`,
   `NDL PCM plane: 2 channel(s) from a 2-channel stream`, and no `frame parts:` warnings.
2. Read `NDL audio output: <MultiChannelPcm> — offering up to N channel(s)` at startup. That one
   line decides whether 5.1 is offered at all, and it has never been seen on a real set.
3. If 5.1 is offered, listen for channel order. `NDL_51_ORDER` is `[0,1,4,5,2,3]`, inferred from
   ss4s; if dialogue lands in the surrounds, try identity `[0,1,2,3,4,5]`. Audible, not silent.
4. Watch for `frame parts:` warnings and picture corruption — that is the whole test of whether NDL
   tolerates a fragmented AU. Reverting means forcing `Negotiated::clamp`'s `frame_parts` to false.
5. Confirm audio survives a `loss → freeze → reanchor` cycle. This path has a documented history of
   permanent session mutes and it is now the primary audio route.
6. Decide 7.1: keep the fold, or drop `PLANE_MAX_CHANNELS` handling entirely.

## Key decisions
- The trim keeps host-PTS spacing and removes only the constant; ss4s's "stamp arrival time"
  approach would also discard the spacing NDL paces on (NOTES § "NDL's audio plane").
- Trim ramps rather than steps: `raw` advances one frame interval per frame, so a one-shot debt
  would emit a stamp behind its predecessor.
- Trimming must finish **before** the audio plane latches, because `play_audio` can only move a
  stamp forward. Hence a 600 ms settle and a deferred latch, rather than disarming the trim on
  audio-carrying routes (the first design).
- `AudioRoute` is picked before the load (it decides the plane's format) and `on_plane()` downgrades
  to `Software` if the load produced none. Opus offload wins where opted in and stereo.
- A 7.1 session loads a 6-channel plane; `PcmFeed::plane_channels` and `AudioRoute::plane_config`
  must agree or NDL reads the interleave at the wrong stride.
- PCM struct is byte-exact with ss4s: `layout` **null**, `sampleRate` is the enum (1 = 48 kHz) not
  hertz, 24 of the union's 32 bytes with the rest zero-filled.
- Plane silence is format-specific — a PCM plane fed `opus_empty_frame_211` reads 3 bytes of samples.
- A broken AU part is reported as loss, reusing freeze-until-reanchor rather than a new path.
- `stats.frames` counts completed AUs only, so the overlay's fps stays pictures per second.

## Dead ends
- The video path has no copies to remove: core reassembles one contiguous `Vec` (which
  `NDL_DirectVideoPlay` requires) and `sink::submit` passes that pointer straight through.
- "PCM removes the software Opus decoder" — it does not. `PcmFeed` *is* libopus; only the sink moved.
- `NDL_DirectAudioRegisterCallback` as a 5.1 gate (ss4s's webOS 7 version proxy): replaced by the
  real capability query, which also distinguishes "capable but Sound Out isn't passthrough".
- `PcmFeed::new` via `?` inside `spawn_plane_threads`: an early return there detaches the clock
  thread still feeding NDL. Folded into the spawn's `io::Error` instead.

## Gotchas
- **Native `cargo check` silently no-ops** (config pins the armv7 cross target) — it printed
  "Finished" over a deliberately broken file. Use `task docker:check` / `docker:lint` only.
- `git checkout <file>` cost a full round of pump.rs edits once. Prefer targeted reverts.
- Unit tests ship in-tree but cannot run off-device: `cargo test` links the whole binary and
  `-lNDL_directmedia` exists only in the cross sysroot.
- Audio now starts ~600 ms into a session by design (the deferred latch); the pump logs the gap.
  That is not a bug report.
- `NDL_DirectAudioSupportMultiChannel`'s return codes are **off by one** against the
  `NDLMultiChannelPCMCallback` codes the header documents.
