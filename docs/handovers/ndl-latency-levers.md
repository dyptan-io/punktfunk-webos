# Three NDL/SS4S latency levers: PTS lead trim, slice-progressive feed, PCM audio plane

**Original request:** review how streamed video and audio are fed into the NDL pipeline and find
latency improvements, researching NDL and ss4s implementation details — avoid frame-data copies,
pass through directly to the decoder, software audio being a separate story. Then: implement all of
it except the "small, probably not worth acting on yet" items, plus a latency estimate.
**Branch:** `ndl-latency-levers` - **Status:** ready for review, UNVERIFIED on hardware

## Scope
Three changes, all opt-in except the first. Explicitly out of scope (user deferred): the
`lock_ffi` contention between video feed and audio plane, and arming the `AvSync` loop.

## Done
- **PTS lead trim** (`session/timeline.rs`, `session/sink.rs`, `session/connect.rs`): `HostPtsAnchor`
  measures its own lead, takes the min over 500 ms windows, subtracts it ramped at ¼ frame interval
  per frame, only within 3 s of an anchor. `SinkConfig::trim_pts_lead` gates it; `sink::pts_trimmed_ms`
  reaches the video heartbeat. Four in-tree tests.
- **Slice-progressive feed** (`session/pump.rs` `AuParts`/`PartStep`, `FrameFlags::partial`,
  `Negotiated::frame_parts`): `frame_parts` now negotiated per `Settings::ndl_frame_parts`, clamped
  to NDL v2 non-SMP. Sink skips `video_e2e`/decode report/audio latch on a non-final piece.
- **PCM audio plane** (`ndl/ffi.rs` `AudioPcmInfo`, `ndl/v2.rs` `NdlAudioConfig` enum + `silence()`,
  `platform/webos/audio.rs` `PcmFeed`, `pump::ndl_pcm_audio_pump`): software Opus decode feeding
  `NDL_DirectAudioPlay` as S16LE on the video timeline. `AudioRoute {Software, NdlOpus, NdlPcm}` in
  `connect.rs` replaces the `audio_offloaded` bool everywhere (`runtime/session_ext.rs`,
  `runtime/stream.rs` overlay tags `Opus SW`/`Opus HW`/`PCM HW`).
- Two Experimental rows ("Audio via TV sink", "Progressive feed") through `app/menu.rs`,
  `app/{state,view}/experimental.rs`, `app/render/{key,prepare}.rs`, `core/model.rs`.
- `docs/NOTES.md` § "Latency levers on the NDL path" — reasoning, ⚠ traps, unknowns.
- `task docker:check` + `docker:lint` clean, `task fmt` applied.

## Left
1. On-device: read the `pts lead: trimming Xms` INFO line and `pts_trim=` on the video heartbeat.
   If it reads ~0 the trim is a no-op on that link and only the other two matter.
2. On-device: toggle "Progressive feed". Failure mode is immediate visible corruption or
   `frame parts:` warnings — that is the answer to "does NDL take a fragmented AU".
3. On-device: toggle "Audio via TV sink" and compare against plain offload and the SDL ring.
   Unknown: the depth NDL keeps on the plane (no query exists for it).
4. Watch the `A/V` overlay figure across a trim on the software route — a cut on the video leg
   shows up as fresh audio lateness, which is the argument for arming `AvSync`.

## Key decisions
- The trim keeps the host-PTS spacing and removes only the constant. SS4S's approach (stamp
  `now - mediaLoadedTime`, never hold) would also drop the spacing NDL paces on, which
  `docs/NOTES.md` § "NDL's audio plane" measures as load-bearing above 1080p.
- Trim ramps rather than steps: `raw` grows one frame interval per frame, so a whole debt taken at
  once would emit a stamp behind its predecessor.
- Trim is **disarmed on both NDL-plane audio routes** — audio stamps there ride this mapping's
  offset and `play_audio` can only move forward (a rewind mutes the session for good), so pulling
  video earlier would land as lip-sync error instead.
- `AudioRoute` is picked *before* the load (it decides the plane's format) then `on_plane()`
  downgrades to `Software` if the load came back without one. Offload wins a double opt-in.
- PCM struct is byte-exact with ss4s: `layout` left **null**, `sampleRate` is the enum (1 = 48 kHz)
  not hertz, 24 of the union's 32 bytes with the rest zero-filled.
- Plane silence is format-specific now — a PCM plane fed `opus_empty_frame_211` reads it as 3 bytes
  of samples.
- 5.1 PCM gated on the `NDL_DirectAudioRegisterCallback` symbol probe (ss4s's own webOS 7 test);
  otherwise the route falls back to software rather than loading a plane the set can't take.
- A broken/abandoned AU part is reported as **loss**, reusing freeze-until-reanchor rather than
  inventing a recovery path.
- `stats.frames` counts completed AUs only, so the overlay's fps stays pictures per second.

## Dead ends
- The video path has no copies to remove: core reassembles one contiguous `Vec` (which
  `NDL_DirectVideoPlay` requires) and `sink::submit` passes that pointer through. Audio chunks are
  already pooled. The user's "avoid memory relocations" instinct was already satisfied.
- `PcmFeed::new` inside `spawn_plane_threads` cannot use `?` — an early return there detaches the
  clock thread that is still feeding NDL. Folded into the spawn's `io::Error` instead.

## Gotchas
- **Native `cargo check` silently no-ops here** (config pins the armv7 cross target). It printed
  "Finished" over a deliberately broken file. Use `task docker:check` / `docker:lint` only.
- Timeline/sink unit tests ship in-tree but cannot run off-device: `cargo test` links the whole
  binary and `-lNDL_directmedia` exists only in the cross sysroot.
- Latency estimate given to the user: trim 0-40 ms (link-dependent), progressive feed ~5-13 ms at
  60 Hz concentrated in the tail, PCM plane net 5-25 ms on the audio leg only.
