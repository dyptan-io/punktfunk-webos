# Media pipeline rework

**Status: implemented on `ndl-latency-levers` (phases 0-5, one commit each). What follows is the
plan as written; the deviations from it are listed at the bottom.**

Goal: one abstract pipeline — sources, processing stages, sinks — so an A/V change is a stage
swap, not an edit across `connect`/`pump`/`sink`/`ndl::v2`. Secondary and equal: fewer copies,
shorter path from wire to sink, lower latency. This also re-judges every change on
`ndl-latency-levers`, since not all of it is confirmed beneficial.

## Part 1 — verdict on the branch

| Change | Verdict | Action |
| --- | --- | --- |
| PTS lead trim (`timeline.rs`) | **Keep.** Only lever with numbers (35-39 ms). | Becomes a `LeadTrim` policy on the session clock. |
| Deferred audio latch (`ready_for_audio`) | **Keep**, but it is a workaround for the clock having no owner. | Folded into `SessionClock::settled()`. |
| Slice-progressive AU feed | **Unproven on hardware.** Corruption risk sits on the critical path. | Keep the code; gate on `SinkCaps::partial_au` + auto-disable after repeated part breaks. |
| PCM plane as **default** audio route | **Reject as default.** Field report: stream goes laggy — the same class of fault the silent clock plane fixed. Feeding the pacing plane from the network makes NDL's pacing depend on audio arrival jitter. | Default back to software Opus -> SDL + silent clock plane (last known-good pacing shape). All three routes stay, user-selectable in Settings, swappable without touching the pipeline. |
| Deletion of `JitterPolicy` / `AvSync` / crossfade shed | **Keep deleted** — but the fallback ring is the default again, and a fixed 25 ms prime is what it has. | Add a minimal, portable de-jitter target inside `AudioStage` (sink-blind), not a resurrection of the SDL-specific policy. |
| Channel clamp from `NDL_DirectAudioSupportMultiChannel` | **Bug as wired.** It feeds `core::caps`, so a TV whose *plane* reports stereo also loses 5.1 on the SDL route, which never touches the plane. | Per-route audio caps — see Part 2b. |
| 7.1 -> 5.1 fold in `PcmFeed` | **Delete.** Mixing down silently is exactly what should not happen. | Offer a layout only where the selected route carries it natively; otherwise it is blocked in Settings with a reason, and the host is asked for a width the sink takes verbatim. |
| `AudioRoute` enum threaded through `connect`/`sink`/`pump` | **Replace.** Three arms restated in five places. | Route = which `AudioSink` was constructed; format comes from the sink. |
| `VideoPlayer` enum owning audio-plane accessors | **Replace.** The video type exports `ndl_audio_handle`, `latch_pts_offset`, `audio_plane_lead_ms`. | Split into `VideoSink` + `AudioSink` + `MediaClock`. |

## Part 2 — the pipeline

Three layers, deps inward, matching the existing architecture rule.

### Sink layer (`platform/webos`, hardware boundary)

```rust
trait MediaClock { fn now_ns(&self) -> u64; }           // NDL v2, SMP. V1 has none.

trait VideoSink {
    fn caps(&self) -> VideoSinkCaps;                     // pts, partial_au, flush, render_queue
    fn feed(&self, au: &[u8], pts: SinkTime) -> Result<()>;
    fn flush(&self) -> Result<()>;
    fn queue_depth(&self) -> Option<u32>;
    fn set_color(&self, meta: Option<&HdrMeta>, color: ColorInfo) -> Result<()>;
    fn clock(&self) -> Option<&dyn MediaClock>;
}

trait AudioSink {
    fn format(&self) -> AudioFormat;                     // Opus{ch} | PcmS16{ch, rate}
    fn feed(&self, buf: &[u8], pts: SinkTime) -> Result<()>;
    fn depth_ms(&self) -> Option<i64>;
    fn keepalive(&self) -> Option<&dyn PacingPlane>;     // NDL's fed-plane requirement
}
```

Implementors: `NdlV2Video` (VideoSink + MediaClock), `NdlAudioPlane` (AudioSink, Opus or PCM,
owns the monotonic-stamp ceiling and the silence metronome), `SmpVideo`, `NdlV1Video`,
`SdlAudioDevice` (AudioSink, PCM). The audio plane stops being a method surface on the video
handle; the shared `NdlVideo` handle becomes an implementation detail of the two impls.

### Stage layer (`session::media`, portable, sink-blind)

- **`SessionClock`** — the single owner of host-PTS -> sink-clock mapping: `HostPtsAnchor`,
  `LeadTrim`, settle gate, published offset. Both stages read it; today this state is split
  between `timeline.rs` and four atomics inside `ndl::v2`.
- **`VideoStage`** — today's `NdlSink` minus the backend switch: loss gate / freeze-until-reanchor,
  keyframe throttle, backlog metering + cushion, ABR decode figure, and `AuParts` (moved down out
  of `pump.rs`; AU reassembly is pipeline state, not wire state).
- **`AudioStage`** — decode (or passthrough), concealment, layout fold, de-jitter target. Emits
  whatever `AudioSink::format()` declares. One implementation covers all three of today's routes.
- **`PacingPolicy`** — the fed-plane keep-alive, an object the pipeline owns rather than
  `NdlVideo::run_clock_plane`.

### Pipeline layer (`session::pipeline`)

`MediaPipeline::build(&Negotiated, caps) -> Pipeline` selects sinks, wires stages, owns the
threads and one shutdown ordering. `connect.rs` shrinks to handshake + build; `pump.rs` shrinks to
transport drain calling `pipeline.video()` / `pipeline.audio()`.

## Part 2b — audio routes and channel layouts

Three routes, first-class and swappable: **Software** (Opus -> SDL, silent clock plane),
**NDL PCM** (Opus decoded here -> NDL's PCM plane) and **NDL Opus** (raw stream -> NDL's Opus
plane). Selecting one is a Settings row; adding a fourth is one `AudioSink` impl plus one enum
variant, with no change in `connect`, the stages or the pump.

**No mixing down, ever.** The host is asked for exactly the width the selected sink can output.
Each route publishes its own caps:

```rust
struct AudioCaps { max_channels: u8, why: ChannelLimit }   // per route, queried at startup
```

- Software: what SDL opens on this set (up to 7.1).
- NDL PCM: `NDL_DirectAudioSupportMultiChannel` — 6 or 2. 7.1 is not offered, not folded.
- NDL Opus: 2. NDL's Opus struct has no multistream mapping field.

`core::caps::max_channels` becomes route-aware rather than one global clamped by the plane. Its
three readers (`connect`, the Settings dropdown, `Settings::clamp_to_caps`) all read it through
the *selected* route, so what is advertised on the wire and what the sink can take are the same
number by construction.

**Settings shows the whole ladder and blocks what this TV can't do.** Today the Audio row locks
wholesale (`RowLock::StereoOnly` / `OffloadStereoOnly`). Instead the channel dropdown lists every
layout, with unsupported entries non-selectable and captioned with the reason — "Your TV's audio
output carries stereo only", "NDL's audio plane carries up to 5.1", "Audio offload is stereo only".
`row_lock` grows a per-*option* sibling (`option_lock`) so the ladder stays visible; a document
carried from a more capable TV is still normalised by `Settings::clamp`.

## Part 3 — latency and copies

1. **Audio decode to i16 directly.** `PcmFeed` decodes to an f32 scratch, then converts and pushes
   into a `Vec<u8>`. `decode()` (i16) into a reusable buffer removes a full per-packet pass; the
   stereo path then decodes straight into the output buffer with zero further copies. The permute
   /fold pass survives only for 5.1.
2. **One audio thread, not two.** The drain thread and the clock-plane thread both wake on a
   millisecond cadence on a 2-3 core SoC; the keep-alive is a timeout branch of the drain loop.
3. **`lock_ffi` split.** Video feed and audio feed serialise on one guard today; audio bursts sit
   in the picture's way. Separate guards per NDL entry point, measured before and after.
4. **Video path is already copy-free** (core hands one contiguous `Vec` through to `play`). The
   remaining video lever is the partial-AU feed, which stays gated until it is measured.

## Part 4 — order of work

0. Re-default the audio route to software + silent clock plane; fix the caps clamp. Small, shippable,
   addresses the regression first.
1. Introduce the traits; move NDL v1/v2, SMP, SDL behind them. `NdlSink` -> `VideoStage`. No
   behaviour change; on-device smoke test.
2. `SessionClock`: pull the mapping/latch atomics out of `ndl::v2` into one owner.
3. `AudioStage` + sink-driven routing: three selectable routes, per-route `AudioCaps`, per-option
   Settings locks, and the 7.1 fold deleted.
4. `MediaPipeline` assembly; shrink `connect.rs` and `pump.rs`.
5. Copy and thread work (Part 3), each measured on the TV separately.

Each phase is `task docker:check` + `docker:lint` clean and deployable on its own; nothing after
phase 0 changes the shape of the stream until it is measured.

## Verification

Unit-testable off device after the split: `SessionClock` (already has five tests), `AuParts`,
`AudioStage` fold/permute/concealment, cushion arithmetic. Everything else stays an on-device
checklist — see the handover's "Left" list, which phases 0 and 3 inherit.

## What shipped, and where it deviates

Phases 0-5 landed as six commits. Two changes to the plan:

- **Per-option dropdown locks were not built.** The channel dropdown lists only what the selected
  route can play (so an unsupported layout is not selectable — the requirement), and the Audio row
  carries a caption naming which limit shortened the list (`menu::audio_limit_reason`). Greying an
  entry *inside* an open dropdown would have meant new widget state, hit-testing and focus-skipping
  for no additional user-visible fact.
- **Two Part 3 levers were deliberately skipped**, both recorded in `docs/NOTES.md`: splitting
  `lock_ffi` per plane (no NDL entry point is documented thread-safe — a second guard is a guess
  about vendor internals) and merging the clock plane's 20 ms keep-alive into a pump that parks up
  to 100 ms (a starved plane is the stutter the plane exists to prevent).

Everything else is as described: `core::media` holds the traits, `session::{audio,stage,pipeline}`
the stages and assembly, `AudioRoutePref` the user-facing route pick, and no path folds a layout
down any more.

## On-device checklist

1. **Each route in turn** (Settings → Audio output): confirm the log's `audio path:` line, then
   listen. Software is the baseline; PCM and TV-decoder are the ones under test.
2. **Watch for the lag report that started this.** `video: … plane_lead=` in the debug heartbeat is
   the plane's depth; sagging toward zero under a plane route is the stutter signature.
3. **5.1**, where the TV offers it: check channel order before anything else (`NDL_51_ORDER`).
4. **`frame parts:` warnings** and picture corruption — still the open question about NDL taking a
   fragmented AU.
5. **Loss → freeze → reanchor with audio still alive**, on every route. This path has a history of
   permanent session mutes.
