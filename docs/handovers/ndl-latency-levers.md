# NDL latency levers: PTS lead trim, slice-progressive feed, and the two audio routes

**Branch:** `ndl-latency-levers` — **Status:** builds clean, reviewed, **not run on hardware.**

**Original request:** review how streamed video and audio are fed into the NDL pipeline and find
latency improvements, researching NDL and ss4s. Then implement them, and reduce the route count to
what the TV can actually play.

This doc is the current state. The blow-by-blow of the PCM route (built, measured, deleted) and the
phase-by-phase pipeline rework are in git history on this branch and in
`docs/MEDIA-PIPELINE-PLAN.md`; neither is repeated here.

`task docker:lint` exit 0 · `task docker:test` 89 passed · `task fmt` applied.

---

## What shipped

**Video**
- **PTS lead trim** (`session/timeline.rs`). `HostPtsAnchor` measures its own standing lead,
  subtracts the minimum over 500 ms windows, ramped at ¼ frame interval per frame, inside a 600 ms
  settle. The anchor otherwise bakes frame 0's own delivery latency into the whole run, and frame 0
  is the session's worst frame (it follows the handshake and the ABR probe).
- **`ready_for_audio()`** holds the audio plane's latch until the trim settles. Audio stamps ride
  this offset and can only move FORWARD — a rewind mutes the session permanently — so the trim must
  finish before audio joins.
- **Slice-progressive feed** (`session/stage/parts.rs`). AU prefixes reach the decoder while the
  rest is on the wire. On for every NDL v2 session. Enforces core's part contract; a break is
  reported as loss, reusing freeze-until-reanchor.

**Audio** — two routes: `Software` (SDL, default) and `NdlOpus` (offload, stereo only).
- The SDL path runs `punktfunk_core::audio::JitterPolicy` (`JitterTuning::AAUDIO`, unmodified) —
  adaptive floor, crossfaded drift shed, near-miss growth, hollow de-priming. Removed earlier on
  this branch and **restored**; see below. `AvSync` stays deleted.
- `AudioStage` decodes only to f32, which is what libopus emits and what SDL takes: no conversion
  pass, no second buffer.
- `ffi::multichannel_pcm_status` / `ndl::audio_output_width` narrows the wire request on the
  software route too — asking for 5.1 the TV would only fold down wastes airlink and host CPU.
- `AudioPlane::lead_ms` is the video heartbeat's `plane_lead=`.

**Structure** — video backends behind `VideoSink`/`AudioSink`/`AudioPlane`/`MediaClock`
(`core::media`), one `SessionClock` both planes stamp against, `MediaPipeline` owning assembly.
`session/stage/` is `{mod,metrics,parts}.rs`.

---

## Which levers are real

| Lever | Verdict |
| --- | --- |
| PTS lead trim | **Real.** Min-over-window is by construction slack no frame in the window needed; the ramp keeps stamps increasing. Measured 35.7 ms / 38.8 ms on two CX sessions. |
| Serving from exactly the prime depth | **Superseded.** It was real, but `JitterPolicy`'s drift shed regulates the same overshoot *and* every later source of it, so the policy owns priming now rather than being second-guessed for one transient. |
| f32-only audio decode | **Real.** One conversion pass and one buffer gone per packet. |
| Slice-progressive feed | **Unproven, and mode-dependent — see below.** |
| Deleting `JitterPolicy` | **Reverted — it was not a win at all.** See below. |
| Deleting `AvSync` | **Kept deleted.** It never steered: gated behind an unmeasured `$HOME/av-trim-ms.conf`, against a video reference biased low by NDL's unobservable decode+panel term. |

### Slice-progressive delivery is inert on small AUs and costs a copy on large ones

Read `packet/reassemble.rs` in core before judging this. Early parts are emitted only for an AU
spanning **more than one FEC block** (`MIN_STREAM_BLOCK_SHARDS = 16`, so a block is ~19 KB), and the
cursor deliberately stops short of the final block. Consequences:

- **A 1-2 block AU emits no early part at all.** At 1440p120 / 25 Mbps a delta frame averages ~26 KB
  — the feature never fires, and costs nothing.
- **When it does fire it adds a full-AU copy.** Non-final parts are `buf[lo..hi].to_vec()` and the
  completing part is `done.buf[lo..].to_vec()`; with no early part, `done.buf` is *moved* instead.
  So the handover's old "the video path has no copies to remove" is true only with parts OFF —
  turning them on introduces one whole-AU copy plus one allocation per part. At 1080p60 / 25 Mbps
  (~52 KB/frame, ~3 blocks) that is every frame, roughly 3 MB/s of memcpy.
- **The benefit is unverifiable from the app.** It only pays off if NDL *begins decoding* a partial
  AU rather than buffering it until complete. NDL is submit-only and says nothing. This is the one
  thing on-device testing has to settle.
- The video heartbeat now logs `parts=` next to `frames=` so a session log answers "did this even
  fire" without guesswork.

Not a bug: feeding the same `frame_index` N times is safe against core's RFI detector — the repeats
land in `observe`'s "straggler behind the delivery point" arm and report no gap.

---

## Open decisions

~~1. The jitter ring.~~ **Resolved: restored.** The removal was credited with "~35 ms of floor" and
   that was wrong — the old local `WEBOS_TUNING` was `base_target_ms: 25` / `max_target_ms: 90` and
   the fixed prime that replaced it was also 25/90, so no floor was ever saved. What was lost was
   jitter resilience on the fleet's worst link. The ring now runs `JitterPolicy` again, with two
   changes from the pre-removal wiring:
   - **`JitterTuning::AAUDIO` instead of a local preset.** Field for field it already is what
     `WEBOS_TUNING` was, with the old `deprime_after: 5` **callbacks** re-expressed upstream as
     `deprime_ms: 60` (core moved that fuse to time because a callback count means a different span
     on every device). A preset tracking upstream is one fewer thing to hand-tune.
   - **`AvSync` is not coming back.** `set_sync_target` is never called, which core documents as
     reproducing unsynchronised behaviour exactly.
   Core also grew near-miss growth, hollow de-priming, shrink probes and a *faded* hard trim since
   this client last used it; all inherited free. New debug line:
   `buffer_ms target_ms primed underruns sheds trims dropped_ms`.

2. **The trim de-syncs the DEFAULT route.** It pulls the picture ~36 ms earlier; nothing pulls SDL
   audio earlier, because that ring ignores PTS entirely and plays in arrival order. Net: audio lags
   video by ~36 ms more than before on Software. On `NdlOpus`, `PLANE_LEAD_MS` (40 ms) cancels it by
   design. `PRIME_MS` is the lever, but it is a listen-on-device call, so it is untouched.
3. **Slice-progressive auto-disable was never implemented.** The plan asked for "auto-disable after
   repeated part breaks"; today a break is reported as loss and the feature stays on. Reverting
   means forcing `Negotiated::clamp`'s `frame_parts` to false.
4. **7.1** — keep `PLANE_MAX_CHANNELS` handling or drop it.

---

## On-device checklist

1. Stereo session: expect `audio path: software Opus decode -> SDL2 + NDL clock plane`.
2. Read `SDL audio device:` at open — the negotiated quantum silently raises the effective prime to
   `max(PRIME_MS, one callback)`. Nothing about this route's latency can be concluded without it.
3. Watch `parts=` vs `frames=` in the video heartbeat. Zero parts means slice-progressive is inert
   on this mode; a nonzero ratio means the copy cost is being paid and the benefit is now worth
   measuring for picture corruption.
4. Confirm audio survives `loss → freeze → reanchor`. This path has a documented history of
   permanent session mutes.
5. Read `audio playback (SDL device)`. `target_ms` above 25 means the adaptive floor grew, i.e.
   this set genuinely needed more slack than the base — the single clearest justification for the
   policy being back. `sheds` vs `trims` separates drift corrected inaudibly from the link
   outrunning the headroom.
6. Audio starts ~600 ms into a session **by design** (the deferred latch). Not a bug report.

---

## Gotchas

- **Native `cargo check` silently no-ops** (config pins the armv7 cross target) — it printed
  "Finished" over a deliberately broken file. Use `task docker:check` / `docker:lint`.
- **`task docker:test` DOES run the tests off-device** (Linux target, 89 passing). An earlier
  revision of this doc claimed tests cannot run off-device; that is only true of bare `cargo test`,
  which links the whole binary and needs `-lNDL_directmedia` from the cross sysroot.
- **`HostPtsAnchor` monotonicity is load-bearing and was one repeated PTS away from breaking.** A
  delivery whose host PTS does not advance while the ramp still owes trim used to emit a stamp
  ~4 ms behind its predecessor — the permanent-mute failure. Now clamped against the last base.
  Note the obvious test for this is **vacuous**: the repeat must land mid-ramp, after the target is
  set and before `trim_ns` reaches it.
- `git checkout <file>` cost a full round of pump.rs edits once. Prefer targeted reverts.
- `NDL_DirectAudioSupportMultiChannel`'s return codes are **off by one** against the
  `NDLMultiChannelPCMCallback` codes the header documents.

---

## Key decisions worth not re-litigating

- The trim keeps host-PTS spacing and removes only the constant; ss4s's "stamp arrival time"
  approach would also discard the spacing NDL paces on.
- The trim ramps rather than steps, because `raw` advances only one frame interval per frame.
- `AudioRoute` is picked before the load (it decides the plane's format); `on_plane()` downgrades to
  `Software` if the load produced none.
- **Every V2 load asks for a plane.** NDL only paces the picture against a fed audio plane, so a
  session on the software route still runs the silent metronome.
- A broken AU part is reported as loss, reusing freeze-until-reanchor rather than a new path.
- `stats.frames` counts completed AUs only, so the overlay's fps stays pictures per second, and
  `feed_us` is now accumulated across an AU's pieces for the same reason.

## Dead ends

- **"PCM removes the software Opus decoder"** — it does not. Only the sink moves.
- `NDL_DirectAudioRegisterCallback` as a 5.1 gate: replaced by the real capability query.
- `PcmFeed::new` via `?` inside `spawn_plane_threads`: an early return there detaches the clock
  thread still feeding NDL.
- **The `lock_ffi` contention between the video feed and the audio plane is still unexamined.** The
  clock plane takes the lock every 20 ms for a burst of up to 8 `audio_play` calls, and the video
  feed takes it per part. Deferred deliberately; measure before touching.
