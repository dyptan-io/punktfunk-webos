# Architecture & platform gotchas

Verified against LG CX (webOS 5.6) and G5 (webOS 10.3). Load-bearing decisions only.

## Toolchain

- Cross target `armv7-unknown-linux-gnueabi` (tier-2) + webosbrew toolchain. Linux-aarch64-only; CI native, dev Docker.
- `.cargo/config.toml` wires linker to `scripts/cc-shim.sh` (passes `--sysroot` explicitly).
- **Soft-float was single biggest perf fix** (~300ms → ~30ms per render). Non-`hf` target spec disables hardware FP codegen despite VFP3/NEON existing. Fix: `target-feature=+neon,+vfp3,-soft-float` + `target-cpu=cortex-a73` in `.cargo/config.toml`. Changes *codegen* only, not FFI ABI.
- **glibc shims required** (`src/glibc_compat_shim.c`): webOS glibc ~2.12 predates `getauxval`/`gettid`/`sendmmsg`. Linked via `cargo:rustc-link-arg`, **must land AFTER libstd** (single-pass linker drops `link-lib=static` too early).
- **SDL2 must be webosbrew fork** (release-2.30.12-webos.5, not generic SDL2). Only fork has Wayland shell-integration (`QT_WAYLAND_SHELL_INTEGRATION=webos`). On-device system copy is 2.0.10 (too old). Bundle own libSDL2 with `$ORIGIN/../lib` RPATH (set in `build.rs`).
- **cmake/opus**: `punktfunk-core`'s `quic` feature needs CMAKE_POLICY_VERSION_MINIMUM=3.5 (modern CMake refuses vendored libopus's old minimum).

## UI rendering

Hybrid software/GPU: `tiny_skia` rasterizes tiles, SDL2 composites. Redraw-on-change (no every-tick render). Key facts:

- **Never use `tiny_skia::Painter::draw_pixmap/fill_rect` for large areas** (~300ms full-screen). Use `pixmap.data_mut()` loop or `copy_from_slice`. Verify with on-device timing, never assume a call is cheap.
- Tiles use premultiplied-alpha; `Compositor::upload` un-premultiplies (SDL `BlendMode::Blend` expects straight alpha).
- `FilterQuality::Nearest` + `anti_alias=false` are cheaper scan-conversion paths.
- Fonts: Geist (OTF, embedded). Icons: Material Icons subset (~1.7 KB) — **subset, so new `ICON_*` codepoint needs font regenerated** (`assets/icons/NOTICE.md` has `pyftsubset` line + codepoint list). Assume Latin only.
- **Scroll fade needs viewport to cut mid-row, else invisible.** Unfocused rows draw no own background (`draw_selectable_fixed` fills only when focused), so a viewport ending on a row boundary has only card background in last pixels — fading `SIDEBAR_BG` into `SIDEBAR_BG` is a no-op; first attempt shipped rendering nothing. `ui::SETTINGS_PEEK` deliberately leaves partial row for `SCROLL_FADE_H` to dissolve.
- **Modal scrolling is pixel-based, offsets are row-based.** `scroll.offset` stays integral (focus logic + scrollbar defined in rows); `App::modal_scroll_px` is animated *rendered* crop, eased like Home grid. Pixels also let last row sit flush at list end — `offset * stride` overshoots by peek strip. Anything positioned against the list (focus tile, dropdown anchor) **must** derive from same pixel offset: focus tile is focused row re-rendered, so anchoring to quantized row shows that row twice during scroll. Can also hang past viewport mid-glide, hence clip in `draw_list`.

## Video decode (NDL DirectMedia)

- `libNDL_directmedia.so.1` is real device library; NDK sysroot ships link-time stub.
- PTS = milliseconds since `NDL_DirectMediaLoad`, not wall-clock.
- Audio decoded client-side via Opus (not routed through NDL).
- **Decouple decode dimensions from punch-through rect** — else 1080p stream on 4K panel punches only top-left quarter.
- **Loss recovery required** — no periodic IDRs in stream. `video_pump` calls `note_frame_index()` every frame (throttled RFI on gaps) + `request_keyframe()` backstop when `frames_dropped()` climbs.
- **Freeze-until-reanchor adapted for NDL**: NDL does decode+present in one opaque call (no split); client reimplements skip-until-reanchor subset. Forward gap arms `holding` flag; frames withheld until one arrives with `FLAG_SOF` (IDR) or recovery anchor.
- HDR mastering metadata can change mid-session — drain `next_hdr_meta` every frame.
- **`NDL_DirectVideoSetHDRInfo` forces panel into HDR mode on *any* call** (OLED65CX, webOS 5): ignores SDR `transfer`/`primaries` triplet, emits HDR infoframe regardless, so SDR/H.264 stream showed in HDR picture mode. Fix: `ndl.rs::set_color_info` no-ops when `meta` is `None` (SDR) — only genuine HDR mastering metadata reaches NDL. Cost: NDL can no longer fix a bitstream's missing VUI colour info; SDR relies on bitstream VUI. HDR also gated to HEVC end-to-end (`session::connect`: `apply_hdr = host_hdr && codec==H265`; explicit H.264 pick drops HDR caps + hides Settings toggle).

## DualSense feedback: Bluetooth service, not hidraw

Adaptive triggers work on **non-rooted** TV, but not through SDL. Verified end-to-end on G5 (webOS 10.3, dev-mode install, `DualSense` over Bluetooth): trigger resistance, section walls, lightbar colour all confirmed on real hardware.

- **No `/dev/hidraw*` in app jail** — not even with pad connected, no `hidraw`/`leds` class in `/sys` either. So SDL's HIDAPI PS5 driver + `SDL_GameControllerSendEffect` (both in bundled fork) never reach the pad. **Don't re-attempt via SDL, don't bump to SDL3** — no webOS SDL3 fork, and blocker is jail policy, not SDL's API.
- **What works instead**: `luna://com.webos.service.bluetooth2/hid/internal/sendData` writes arbitrary HID output report to pad. Permitted because `compat.api.json` places it in **`public`** API group, and `/usr/share/luna-service2/devmode_certificate.json` grants dev-mode app `["ares.webos.cli", "public"]`. Restricted `devices`/`bluetooth.manage` groups *not* needed.
- **Payload traps** (each cost hours): `reportData` must be int array **with no `reportId` key** — one extra property fails whole call with generic "does not match the expected schema" naming nothing. `setReport` never works (always error 4 "operation can not be performed at this time"); only `sendData` does. `getReport` *hangs* on a pad that doesn't answer, so callers need a deadline.
- **Report must be CRC-signed** exactly as kernel's `hid-playstation`: 78 bytes (`0x31`, seq<<4, `0x10` tag, 47-byte common block, 24 reserved, CRC32-LE), CRC over `0xA2` seed byte plus report body. **Wrong CRC is silently ignored by pad while service still answers `returnValue: true`** — most misleading failure mode here. Don't prepend `0xA2` to `reportData`; stack adds HIDP header itself.
- LG backported `hid-playstation` to kernel 5.4, so pad binds as three input devices (pad/motion/touchpad) sharing one `U: Uniq=` MAC — where `dualsense::find_address` reads it.
- **Rumble does not use this path**: pad's event node advertises `EV_FF`, group-writable by `compositor` (app's uid is in it), so rumble goes through SDL's evdev force feedback, working for any pad. Reports in `dualsense.rs` deliberately never set compatible-vibration valid-flag, so paths can't fight.
- `hid/internal/*` is undocumented vendor surface — feature-detected, failing soft, never assumed.
- **Feedback sends must be throttled, or video plane goes black.** Each send forks/execs `luna-send-pub`, copying page tables of a process holding SDL, decoder + buffers. A Steam/Gamescope host *animates* the `DualSense` lightbar, so unthrottled sender spawned dozens of processes/sec on 2-3 core TV — observed failure was **black panel with frame counter climbing, `dropped=0`, `backlog=0`**: decode thread is priority-boosted so it kept running while compositor never presented (audio underruns the other tell). `dualsense.rs` drops identical states, spaces rest by `MIN_SEND_INTERVAL`. Don't assume host feedback is human-paced; lightbar alone is not.

Host side: a game only emits trigger effects when it sees a `DualSense`, so pad kind in handshake decides whether this feature does anything. Settings' **Controller** row (`store::GamepadType`) defaults to `Automatic`, which **mirrors attached pad** (`gamepad::detect_type`) rather than sending wire `GamepadPref::Auto` — that wire value means "host decides", and host decides Xbox 360, which is why a `DualSense` first showed as Xbox pad with no effects. Resolution happens per session (`main::resolve_gamepad_type`), deliberately doesn't write back, so stored preference keeps meaning "match my pad". Host env `PUNKTFUNK_TEST_FEEDBACK` makes host send scripted lightbar/LED/trigger burst — use to test without a game.

## Known platform limitations (don't retry)

- **Frame rate paces the stream; can't set panel refresh rate.** `webosbrew/SDL-webOS` exposes read-only `SDL_webOSGetRefreshRate` only; no set-side webOS API. Used by `PtsPacer` (`session.rs::reconciled_pace_interval_ns`): when measured panel Hz is within ±2 Hz of stream fps, paced PTS grid anchors to panel's cadence instead of stream's (aurora-tv's `session_worker.c` trick). Still not real vsync — just PTS quantization to display's rate.
- **Magic Remote Back requires `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK`** set before window creation. Arrives as `keycode = 2097155`. Same for Home (`SDL_WEBOS_ACCESS_POLICY_KEYS_HOME`) and Guide (`SDL_WEBOS_ACCESS_POLICY_KEYS_GUIDE`). Launcher ribbon overlay needs `SDL_WEBOS_ACCESS_POLICY_RIBBON=false` or it pops over the app.
- **A held Back arrives as EXIT key, not a long Back — don't time the hold yourself.** webOS does long-press detection: short Back tap delivered as Back key (`keycode 2097155`, no scancode), but *holding* Back fires webOS's own EXIT gesture, delivered as discrete `SDL_SCANCODE_WEBOS_EXIT = 505` press — held Back key itself never reaches app (confirmed on-device: long press logs no Back down/up at all). So "hold Back to open the dialog" can't work by timing Back events; instead poll `WEBOS_EXIT_SCANCODE` (edge-detected like colour buttons — 505 is outside rust-sdl2's `Scancode` enum so never surfaces in safe event API) and open disconnect/quit dialog on its rising edge. Short Back tap stays plain: forwarded to host as Esc (stream) or back-nav (menu). Exactly aurora-tv's split (`keyboard_webos.c`: EXIT→open overlay, BACK→VK_ESCAPE). Needs `KEYS_EXIT` (above) or gesture SIGTERMs instead of delivering 505.
- **Gamepad disconnect shortcuts must be holds, not presses** (`main::DisconnectChord`, 2 s). Guide, both shoulders, or Start+Back opens in-stream disconnect dialog — and every one of those buttons is also forwarded as real game input, which is the whole constraint: L1+R1 in particular is a common in-game binding, so press-to-fire would kill streams mid-play. Chord state tracked from transitions (SDL reports no held-state here), **cleared when it fires or pad unplugs** — an open dialog swallows controller events and an unplugged pad sends no releases, so without that the buttons stay logically down and the dialog reopens the moment it's dismissed.
- **Hidden window gets no pointer input.** Keep it mapped and fully transparent `RGBA(0,0,0,0)` each frame so NDL plane shows through (not `.hide()`).
- **Two independent cursors** — webOS draws local cursor; host draws second over network. `show_cursor(false)` hides SDL's own cursor object during a stream, and that is all `cursor::Cursor` does today. Known-unsolved: **the compositor draws its own pointer and we can't stop it**. `SDL_ShowCursor` can't, and not only because the arrow isn't SDL's — it also only reaches the backend when its cached `cursor_shown` flips, so a repeat hide is a silent no-op and `is_cursor_showing()` (reading that same cache) reports "hidden" while the TV visibly draws an arrow; don't build a guard on that query. The obvious lever, `SDL_webOSCursorVisibility` → `wl_webos_input_manager.set_cursor_visibility`, was implemented (hide on capture, re-assert on pointer activity since webOS re-shows it, restore on exit incl. the panic hook) and **verified not to work on webOS 26** — arrow stays regardless. Kept behind `cursor::COMPOSITOR_CURSOR_CONTROL = false` rather than deleted; flip it to re-enable the whole path. Fixed instead via `EVIOCGRAB` on the mouse's evdev node — see the trade-offs in the HID bullet below. Motion is **not** scaled — an earlier client-side damping factor was dropped as a guess that only ever masked the jitter the evdev path below actually fixes.
- **Absolute pointer input is bounded by the panel; captured streams need relative.** webOS's pointer can't leave the screen, so `MouseMoveAbs` saturates at the edge — the host cursor can't reach a second display, and games wanting continuous motion stall. "Cursor capture" therefore also switches SDL to relative mode and sends `InputKind::MouseMove` deltas. webOS advertises no `zwp_pointer_constraints_v1`, so the SDL fork emulates relative mode by warping its own pointer to screen centre each motion (`wl_starfish_pointer_set_cursor_position`) — which is what makes the deltas unbounded. `SDL_SetRelativeMouseMode` therefore always returns 0 here, even where the standard protocols are absent. The protocol also carries a host-driven relative hint (`PunktfunkCursorState` flags bit 1) that would drive this automatically; the cursor channel is unconsumed so far.
- **A real HID mouse must bypass SDL — `/dev/input/event*` is readable from the jail.** Unlike hidraw (above), the evdev nodes are reachable: `root:compositor 0660`, and the app's uid carries gid 505 (`compositor`) in its supplementary groups — the same access the pad's `EV_FF` rumble node already relies on. Motion arriving via SDL comes from the compositor's pointer, smoothed and resampled for a wrist-waved remote, and jitters in games no matter what the client does with the deltas; `platform::webos::evmouse` reads the mouse directly instead (aurora-tv's "Use Hardware Mouse"). Three things that are not optional once it's live, each observed as a real bug: SDL's echo of *this device* must be **dropped** (the compositor still sees the mouse, so forwarding both double-sends — but drop it by recency of evdev activity, not blanket, or the Magic Remote stops working in-stream since it shares the same SDL pointer; the remote then sends absolute, which suits a device you aim and doesn't need the warp emulation), SDL relative mode must be **off** (the fork warps its pointer per motion event — a thousand pointless compositor round-trips a second), and the reader thread must be **reniced to −10** like the video pump (at nice 0 it lost the CPU to the boosted decode threads for up to 28 ms while a 1 kHz mouse kept reporting, which is jitter indistinguishable from the compositor's). Device filter is `EV_REL` with `REL_X`/`REL_Y` and *not* an absolute pointer — test `ABS_X`/`ABS_Y` specifically, since real mice advertise stray absolute axes (the Logitech receiver here reports `ABS_VOLUME` for its media keys). `EVIOCGRAB` is used, but scoped to `HidMouse::set_active`, tied to capture state, not held for the reader's whole life — the kernel releases the grab the moment our fd closes (including on panic), so a wedged reader thread costs "no HID input," never a TV-wide dead mouse; the surface-manager's own fd stays open throughout and just stops receiving events while ours holds the grab. Same flag also gates whether the reader calls its `sink` at all, so "grabbed" and "forwarded to the host" can't drift out of sync the way two independently-toggled atomics could. `commons-evmouse` (moonlight-tv/aurora-tv's backing lib) ships the same idempotent-per-fd `EVIOCGRAB(1/0)` primitive, confirmed by reading its source — but its own session code never calls it (default open path forces `grab=false`), so this is a novel wiring of a proven primitive, not a ported fix. Opening a node costs ~40 ms on this TV and ~20 nodes are empty (`ENXIO`), so hot-plug rescans are gated on `/dev/input`'s mtime — an unconditional rescan stalls the reader for most of a second.
- **Color buttons: Green/Yellow/Blue need raw scancode polling, Red does not — Red has no usable scancode at all.** `SDL_SCANCODE_WEBOS_{RED..BLUE}=486..489` exist in the fork's `SDL_scancode.h` and `SDL_scancode_webos.c` maps IR keycodes 406..409 onto them, but only 487..489 ever appear in the keyboard-state array (`webos_scancode_down`). Red instead arrives like Back does: a plain `KeyDown` carrying **keycode 2097169** and `scancode: None` (confirmed on-device; `0x200000 + n`, same family as Back's 2097155). So poll the other three, match Red as a keycode. Nothing to unlock — there is no `ACCESS_POLICY_KEYS_*` hint for colour keys.
- **Don't toggle window show/hide while NDL composites.** Silently kills process (uncatchable Wayland crash). Test visibility changes in isolation.

## Runtime gotchas (LG CX/G5)

- Apps install to `/media/developer/apps/usr/palm/applications/<appid>/` = `$HOME` (writable dir for logs, `settings.json`, the art cache and the client identity PEMs).
- `luna-send` over raw ssh **needs `ssh -tt`** (real PTY) or output silently swallowed — the task
  targets go through `ares-install`/`ares-launch` instead, which don't have this problem.
- **Black screen despite decode**: launch through real app lifecycle (`luna-send .../launch`, SAM jailed uid). NDL punch-through only composites for SAM-managed foreground app.
- No env vars in SAM launch, but `params` in `applicationManager/launch` reaches native app as argv[1] JSON (parsed by `src/logger.rs`).
- SDL2/Wayland may report `refresh_rate=0` — clamp to sensible default.

## ChaCha20 over AES-GCM

CX/G5 are 32-bit userland on ARMv8-A. RustCrypto's `aes` crate has ARMv8 intrinsics for `aarch64` only; 32-bit ARM falls back to software regardless. ChaCha20 (add/rotate/xor, no crypto instructions) stays fast. Advertise `VIDEO_CAP_CHACHA20` unconditionally in `session.rs::connect` — only cipher this client speaks.

## Large library handling

- **Tile windowing**: `prepare_tiles` builds tiles only for rows within `CARD_PREFETCH_ROWS` of viewport, at most `CARD_BUILD_BUDGET` per frame. Deliberately larger `CARD_KEEP_ROWS` (hysteresis stops oscillation).
- **Cover art**: `ArtLoader` request/response (UI asks for visible covers, forgets scrolled ones). Cached on disk as *encoded* bytes (`$HOME/art-cache/`, write-then-rename). Failed decodes deleted.
- Effect at 365 titles: retained tiles drop from ~366 to ~40; decoded covers from 365 to viewport window (~5 columns).

## Audio (software Opus path)

Two threads and a ring: `session::audio_feed_pump` decodes Opus and posts chunks down a bounded channel; SDL's audio callback (`platform::webos::audio::RingCallback`) drains that into a ring it owns and serves the device from it under `punktfunk_core::audio::JitterPolicy` — the same de-jitter state machine every other punktfunk client ring runs.

- **The ring primes to 25 ms before the first sample plays**, grows its target under underrun pressure (+10 ms per 3 underruns in a 5 s window, ceiling 90 ms), relaxes after a quiet spell, and sheds drift as **one crossfaded 5 ms frame** once the depth average has sat 20 ms over target for 2 s of consumed audio. Hard cap 120 ms. Preset is `WEBOS_TUNING`, local to `audio.rs`, seeded from core's `AAUDIO` (closest rationale: we own the buffer, and Wi-Fi power-save bunching lands as underruns).
- **Lost packets concealed** with libopus PLC: ask `AudioGapTracker` how many precede current packet, synthesize that many PLC frames first (decode with empty input). Since core v0.26.0 a *single* lost datagram is instead **recovered** from the redundant `0xD2` plane, which core advertises and rebuilds on its own demux side — no client code.
- Depth/target/underruns/sheds are logged from the callback ~every 10 s, and ring depth + A/V offset are on the stats overlay.

**Blind alleys, so they aren't re-tried:**
- **`sdl2::audio::AudioQueue` cannot carry the shared policy.** It exposes `queue_audio`/`size`/`clear` and nothing else — no partial drop — so `JitterStep`'s crossfaded shed is inexpressible against it. The pull callback is a prerequisite, not a refactor.
- **Do not put the drain back on the main loop.** It was there because `AudioQueue` is `!Send`, which put the 5 ms audio cadence behind the UI's software rasterizer; the 500 ms stats-overlay raster was a *documented* underrun source on a 2-core panel.
- **Do not shrink `DEVICE_BUFFER_FRAMES` below 512** to chase latency. The policy owns depth now; a smaller device quantum on this SoC buys more wakeups and more missed callbacks. 512 = 10.67 ms, and `WEBOS_TUNING`'s 25 ms base is sized to clear the `want + 5 ms` device floor it implies.

## A/V sync

The host stamps `pts_ns` on every audio datagram; this client decoded it and threw it away, so the A/V offset was an accident of buffer depths — and it got *worse* every time video got faster (a quicker decode path lowers the video leg and leaves the audio leg alone). `AvSync` now folds it into a measured offset.

**Currently measure-only.** `AvSync::desired_depth` is never called and no target reaches `JitterPolicy`. The blocker is the video reference: NDL is submit-only (`NDL_DirectVideoPlay` reports nothing about presentation), so `session::sink::video_e2e_ns` estimates glass time as *submit instant + render-queue depth × panel interval + a fixed constant*. The first two are measured; the constant — NDL's decode+panel latency after the queue drains — is not observable from the app.

- **Sign, and why it matters:** underestimating that constant by Δ biases the video figure low, the offset high, and aims the ring Δ shallower — i.e. **audio plays Δ early**. A plausible 2-5 frame NDL pipeline is 33-83 ms at 60 Hz, far outside `AvSync`'s 10 ms deadband. Shipping it at 0 and acting on it would be a bigger error than the drift being corrected.
- **How to measure it:** put the stats overlay up (Green) and let the `A/V` figure converge (needs 100 observations and a frame on the glass). That figure is the offset *including* the missing constant, so it reads high by exactly the constant — which is what makes it measurable. There is no knob to write it into: the constant is absent from the estimate entirely, and arming the loop means adding it back to `session::sink::video_e2e_ns` as a compiled-in term plus posting `AvSync::desired_depth` to `JitterPolicy` in `audio::AudioFeed::observe_av`.
- ⚠ **Measure in Game Optimiser mode AND a processing-heavy picture mode.** If those differ much, the constant has to become a real Settings row rather than one compiled-in number. Untested.
- ⚠ **Use `frame.pts_ns`, never the paced value.** Both are in scope at the submit site with near-identical names; the paced one has been mapped into NDL's *player* clock by `HostPtsAnchor`. Using it regulates against a fiction that still looks plausible.
- The estimator's unit tests ship in-tree but **cannot run off-device**: `cargo test` links the whole binary and `-lNDL_directmedia` exists only in the cross sysroot.

## Opus offload to NDL (OFF BY DEFAULT — still freezes 10.3)

⚠ **A second, independent defect on this path, found 2026-08-09 and NOT fixed:** `play_audio` stamps arrival wall-clock while video is stamped host-PTS-anchored + paced, so NDL's own A/V sync regulates two unrelated timelines. Fixing it means both planes sharing one `HostPtsAnchor`, i.e. moving anchor ownership onto the `NdlVideo` handle behind a lock — a change to the working video hot path for a path that is off and broken. Whoever revives offload fixes this first. Symptom would be audio drifting against the picture over a session, not a constant offset.

NDL is the sole video backend. Software Opus→SDL is the audio path; NDL hardware Opus is gated off behind the `NDL_AUDIO_OFFLOAD` const in `session/mod.rs` (flip to `true` to re-test).

The wiring is byte-exact with `mariotaku/ss4s` `ndl/webos5`: `NdlAudioConfig.sample_rate` in **kHz** (`48.0`, not `48000.0`), the stereo `opus_empty_frame_211 = {0xec,0xff,0xfe}` decoder prime fed once right after a successful audio-enabled `NDL_DirectMediaLoad`, combined audio+video in one load, and feed-time PTS (`elapsed since load`, ms) on both audio and video planes — identical to ss4s's `FeedVideo`/`FeedAudio` `GetPts`. Struct layouts (`NDL_DIRECTMEDIA_AUDIO_OPUS_INFO_T`, `..._DATA_INFO_T`) verified field-for-field against the ss4s mock headers.

**Despite full parity it still holds the first video frame forever on the tested G5 (webOS 10.3).** The audio-enabled load returns success, so no runtime probe can distinguish a TV that offloads from one that dies silently — the only safe move is not taking the path by default. Kept opt-in for continued testing on other models; the new `NDL load state:` log (`LOADCOMPLETED`/`PLAYING`) is the signal for whether the present pipeline ever starts.

## ABR startup probe: 2 Gbps, upstream-hardcoded

**"Automatic" bitrate fires a 2 Gbps burst ~2 s into every session, and on Wi-Fi that can cost the session its video entirely** — not a slow start but a flow that never establishes. Measured on G5: a "successful" probe still reported `send_dropped=20211`, i.e. link hammered far past what it can carry (~245 Mbps airlink ceiling), and probes that get nothing back sit on core's 6 s timeout. Capped at 300 Mbps the same link reports `send_dropped=0-167` and stream starts are reliable.

Don't read a slow *start* as this bug — a host compositor coming up has its own startup time, and video legitimately arrives late on first connect of a session. Signal that matters: packet drops on the probe and video that never arrives at all.

This is `CAPACITY_PROBE_KBPS` in `punktfunk-core`'s `client/pump/data.rs` — a **hardcoded const with no cap knob**, still 2 Gbps as of core v0.22.2, directly at odds with this client's own speed test being deliberately capped at 320 Mbps for the same "unbounded firehose starves the app" reason (below).

**Fixed by capping the burst**: `main.rs` sets `PUNKTFUNK_ABR_PROBE_KBPS=300000` before anything spawns a thread (`setenv` isn't thread-safe, and core reads it while building its data-plane pump). 300 Mbps matches what this client already burst-tests its *own* speed probe at, still above the ~245 Mbps airlink ceiling this hardware reaches — measures the link without knocking it over. Knob is core-side; **core v0.22.3 is first release carrying it**, which is why the pin moved off v0.21.0. Against an older core the variable is simply ignored.

That bump also brought a `connect` signature change — a `name: Option<String>` (label the host's pending-approval list shows) between `launch` and `pin`. All four call sites pass `None`, preserving fingerprint-derived label; sending a real TV name is a separate user-visible change.

Blind alleys, so they aren't re-tried:

- `bitrate_kbps == 0` (Automatic) arms **both** the AIMD controller and this probe — client cannot separate them.
- `PUNKTFUNK_ABR_PROBE=0` disables the probe but leaves climb ceiling at negotiated start rate (~20 Mbps), which core's own comment calls a box "Automatic could NEVER climb out of".
- Running our own capped probe instead does **not** work: `request_probe` completes, but `abr.set_ceiling` is only called from core's own probe path (gated on its `capacity_probe_deadline`), so ceiling never moves. No public bitrate/ceiling setter on `NativeClient`.
- Pinning a fixed bitrate also disarms the probe, but costs mid-session adaptation entirely.

## Network speed test quirks

Burst is 320 Mbps / 3s (not 3 Gbps / 5s) — 3-core Cortex-A9 runs UI thread; unbounded firehose starves app. 320 detects any ceiling that changes clamped recommendation (>~285 Mbps). Probe must advertise `VIDEO_CAP_CHACHA20` like real session (core's `bytes_received` counter increments *after* AEAD decrypt). Measured on G5 Wi-Fi: ~245 Mbps airlink ceiling (MediaTek USB 2.0 Hi-Speed bus), nothing client code can raise. New flows sometimes black-hole ~10-29s (AP/driver setup); `run_speed_probe` waits for first completed video frame (cap 35s) before burst — plane is live and path is warm.

## Video backend: NDL DirectMedia only

NDL DirectMedia is the **sole** video backend (Starfish/SMP and its `libplayerAPIs_C.so` wrapper were removed, along with AV1 — only ever decodable through Starfish). Header signatures from `mariotaku/ss4s`. No decode context handle; all NDL calls are serialized behind `NdlVideo::ffi` mutex (not thread-safe per header).

## NDL generations: v2 (webOS 5+) and v1 (3.5-4.x)

- Same library, different symbol sets. v2: `NDL_DirectMediaLoad`, `NDL_DirectVideoPlay(buf,size,pts)`, `FlushRenderBuffer`, `GetRenderBufferLength`, `SetHDRInfo`. v1: `NDL_DirectVideoOpen / SetCallback / SetArea / PlayWithCallback / Close` — webOS 4 has **none** of the v2 entry points.
- **NDL is dlopen'd, never linked — this is what makes webOS 4 boot.** Full BIND_NOW (`DT_BIND_NOW` + `DF_1_NOW`) resolves every undefined symbol at exec time, so a `DT_NEEDED` on `libNDL_directmedia.so.1` makes the loader refuse to start the process on webOS 4: before `main()`, nothing logged, app bounces to the launcher. That was the shipped 0.15.0 `.ipk`. **Never re-add `#[link(name = "NDL_directmedia")]` or an `extern "C"` NDL block** — no webOS 5 test catches it. `-Wl,-z,lazy` is not the fix (keeps the dependency, weakens RELRO binary-wide).
- Generation chosen by `device::ndl_generation()` from Luna `getSystemInfo`'s `sdkVersion` (the field ss4s constraints are written against): v1 below 5, v2 at 5+ **and when unknown** — every currently-working device keeps its path. `task deploy WEBOS_SDK=4.3.0` forces v1 on a modern TV.
- Two-stage gate, as in ss4s: version picks what to *try*, `dlsym` decides. A miss is a named error, never a fallback to the other generation — on webOS 4 the v2 symbols are absent by construction, so falling back only buys a doomed connect.
- v1 limits: H.264 only, SDR/BT.709, **no PTS input** (frames present as fed, pacing inert), no render-buffer query, no flush, no HDR call. Plane is a fixed 1920x1080 placed once via `SetArea` (no native punch-through sizing), letterboxed by ss4s `FitVideo` verbatim. Never reposition video from the app — UI composites over the underlay.
- Those limits are published by `core::caps` and enforced in three places that must agree: `session::connect` (authoritative — codec/HDR/channels are negotiated before any decoder opens), `ui::settings` (row + option visibility), `store::load` (clamps a document written on a more capable TV).
- Not ported from ss4s, deliberately: `smp-webos4` (HEVC/HDR on webOS 4 via `libplayerAPIs` + `AcbAPI`) — out of scope; `m3_kadp_fix.c`, which `mprotect`s a vendor code page RWX and NOPs two bytes out of an MStar codec-type whitelist to pass non-H.264 types — we only ever feed H.264 on v1.
