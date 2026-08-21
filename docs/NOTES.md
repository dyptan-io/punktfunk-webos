# Architecture & platform gotchas

Verified against LG CX (webOS 5.6) and G5 (webOS 10.3). Load-bearing decisions only.

## Toolchain

- Cross target `armv7-unknown-linux-gnueabi` (tier-2) + webosbrew toolchain. Linux-aarch64-only; CI native, dev Docker.
- `.cargo/config.toml` wires linker to `scripts/cc-shim.sh` (passes `--sysroot` explicitly).
- **Soft-float was single biggest perf fix** (~300ms → ~30ms per render). Non-`hf` target spec disables hardware FP codegen despite VFP3/NEON existing. Fix: `target-feature=+neon,+vfp3,-soft-float` + `target-cpu=cortex-a73` in `.cargo/config.toml`. Changes *codegen* only, not FFI ABI.
- **glibc shims required** (`src/platform/webos/glibc_compat_shim.c`): webOS glibc ~2.12 predates `getauxval`/`gettid`/`sendmmsg`. Linked via `cargo:rustc-link-arg`, **must land AFTER libstd** (single-pass linker drops `link-lib=static` too early).
- **SDL2 must be webosbrew fork** (release-2.30.12-webos.5, not generic SDL2). Only fork has Wayland shell-integration (`QT_WAYLAND_SHELL_INTEGRATION=webos`). On-device system copy is 2.0.10 (too old). Bundle own libSDL2 with `$ORIGIN/../lib` RPATH (set in `build.rs`).
- **cmake/opus**: `punktfunk-core`'s `quic` feature needs CMAKE_POLICY_VERSION_MINIMUM=3.5 (modern CMake refuses vendored libopus's old minimum).

## UI preview (container)

`task docker:deploy` runs the app in a container on a virtual 1080p display and serves it over
VNC (`http://localhost:6080/vnc.html`), so UI work needs no TV and no
host tools beyond Docker. Same image, mounts and cache volumes as the cross-build tasks (it goes
through `toolchain:docker-run` like they do); SDL2, Xvfb, x11vnc and noVNC are apt-installed per
run.

- **Build with the `preview` profile** (release codegen, no LTO). `tiny_skia` rasterizes in
  software; a `dev` build spends tens of ms a frame there and reads as input lag, on top of
  llvmpipe and the VNC round trip. `PROFILE=dev` if the rebuild time matters more.
- **No GPU and no mDNS.** llvmpipe means animation timing here is not the TV's, and Docker's
  "host" is the Linux VM, so `services::discovery` sees no LAN multicast — add hosts by hand.
  Unicast is unaffected, so a hand-entered host pairs and speed-tests for real.
- **Launch params reach the app as the argv[1] JSON SAM sends on a TV**, so `WEBOS_SDK=4.0.0`
  exercises the NDL v1 path here too. Telemetry is wired to a listener inside the container, so
  logs land in the terminal at `TELEMETRY_LEVEL` with nothing to configure.

## UI rendering

Hybrid software/GPU: `tiny_skia` rasterizes tiles, SDL2 composites. Redraw-on-change (no every-tick render). Key facts:

- **Never use `tiny_skia::Painter::draw_pixmap/fill_rect` for large areas** (~300ms full-screen). Use `pixmap.data_mut()` loop or `copy_from_slice`. Verify with on-device timing, never assume a call is cheap.
- Tiles use premultiplied-alpha, and stay that way: `Compositor::upload` sets a composed
  premultiplied blend mode (`SDL_ComposeCustomBlendMode(ONE, ONE_MINUS_SRC_ALPHA, …)`, supported
  by the fork's GLES2 backend) instead of dividing alpha back out per pixel. The un-premultiply
  fallback is still there for a renderer that refuses the mode — it is the path to suspect if a
  tile's alpha looks wrong; `premultiplied texture blending: <bool>` in the log says which ran.
- `FilterQuality::Nearest` + `anti_alias=false` are cheaper scan-conversion paths.
- Fonts: Geist (OTF, embedded). Icons: Material Icons subset (~1.7 KB) — **subset, so new `ICON_*` codepoint needs font regenerated** (`assets/icons/NOTICE.md` has `pyftsubset` line + codepoint list). Assume Latin only.
- **Scroll fade needs viewport to cut mid-row, else invisible.** Unfocused rows draw no own background, so a viewport ending on a row boundary has only card background in last pixels — fading `SIDEBAR_BG` into `SIDEBAR_BG` is a no-op; first attempt shipped rendering nothing. The settings viewport deliberately leaves a partial row for `SCROLL_FADE_H` to dissolve.
- **Modal scrolling is pixel-based, offsets are row-based.** `scroll.offset` stays integral (focus logic + scrollbar defined in rows); the rendered crop is animated in pixels, eased like Home grid. Pixels also let last row sit flush at list end — `offset * stride` overshoots by peek strip. Anything positioned against the list (focus tile, dropdown anchor) **must** derive from same pixel offset: focus tile is focused row re-rendered, so anchoring to quantized row shows that row twice during scroll. Can also hang past viewport mid-glide, hence clip in `draw_list`.

## Video decode (NDL DirectMedia)

- `libNDL_directmedia.so.1` is real device library; NDK sysroot ships link-time stub.
- PTS = milliseconds since `NDL_DirectMediaLoad`, not wall-clock.
- Audio decoded client-side via Opus (not routed through NDL) unless offload is on — see *NDL's audio plane*.
- **`core::caps` has three readers that must agree**: `session::connect` (source of truth, advertised on the wire), `ui::settings` (what's offerable) and `Settings::clamp_to_caps`. A backend that changes the limits changes all three.
- **Decouple decode dimensions from punch-through rect** — else 1080p stream on 4K panel punches only top-left quarter.
- **Loss recovery required** — no periodic IDRs in stream. `video_pump` calls `note_frame_index()` every frame (throttled RFI on gaps) + `request_keyframe()` backstop when `frames_dropped()` climbs.
- **Freeze-until-reanchor adapted for NDL**: NDL does decode+present in one opaque call (no split); client reimplements skip-until-reanchor subset. Forward gap arms `holding` flag; frames withheld until one arrives with `FLAG_SOF` (IDR) or recovery anchor.
- HDR mastering metadata can change mid-session — drain `next_hdr_meta` every frame.
- **`NDL_DirectVideoSetHDRInfo` forces panel into HDR mode on *any* call** (OLED65CX, webOS 5): ignores SDR `transfer`/`primaries` triplet, emits HDR infoframe regardless, so SDR/H.264 stream showed in HDR picture mode. Fix: `ndl::v2::set_color_info` no-ops when `meta` is `None` (SDR) — only genuine HDR mastering metadata reaches NDL. Cost: NDL can no longer fix a bitstream's missing VUI colour info; SDR relies on bitstream VUI. HDR also gated to HEVC end-to-end (`session::connect`: `apply_hdr = host_hdr && codec==H265`; explicit H.264 pick drops HDR caps + hides Settings toggle).

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

Host side: a game only emits trigger effects when it sees a `DualSense`, so pad kind in handshake decides whether this feature does anything. Settings' **Controller** row (`store::GamepadType`) defaults to `Automatic`, which **mirrors attached pad** (`gamepad::detect_type`) rather than sending wire `GamepadPref::Auto` — that wire value means "host decides", and host decides Xbox 360, which is why a `DualSense` first showed as Xbox pad with no effects. Resolution happens per session (`runtime::resolve_gamepad_type`), deliberately doesn't write back, so stored preference keeps meaning "match my pad". Host env `PUNKTFUNK_TEST_FEEDBACK` makes host send scripted lightbar/LED/trigger burst — use to test without a game.

## Known platform limitations (don't retry)

- **Frame rate paces the stream; can't set panel refresh rate.** `webosbrew/SDL-webOS` exposes read-only `SDL_webOSGetRefreshRate` only; no set-side webOS API. Used by `session::timeline::reconciled_frame_interval_ns`: when measured panel Hz is within ±2 Hz of stream fps, the frame interval anchors to the panel's cadence instead of the stream's (aurora-tv's `session_worker.c` trick). That interval is what converts an NDL render-queue depth into time (e2e latency, decode reports).
- **Magic Remote Back requires `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK`** set before window creation. Arrives as `keycode = 2097155`. Same for Home (`SDL_WEBOS_ACCESS_POLICY_KEYS_HOME`) and Guide (`SDL_WEBOS_ACCESS_POLICY_KEYS_GUIDE`). Launcher ribbon overlay needs `SDL_WEBOS_ACCESS_POLICY_RIBBON=false` or it pops over the app.
- **A held Back arrives as EXIT key, not a long Back — don't time the hold yourself.** webOS does long-press detection: short Back tap delivered as Back key (`keycode 2097155`, no scancode), but *holding* Back fires webOS's own EXIT gesture, delivered as discrete `SDL_SCANCODE_WEBOS_EXIT = 505` press — held Back key itself never reaches app (confirmed on-device: long press logs no Back down/up at all). So "hold Back to open the dialog" can't work by timing Back events; instead poll `WEBOS_EXIT_SCANCODE` (edge-detected like colour buttons — 505 is outside rust-sdl2's `Scancode` enum so never surfaces in safe event API) and open disconnect/quit dialog on its rising edge. Short Back tap stays plain: forwarded to host as Esc (stream) or back-nav (menu). Exactly aurora-tv's split (`keyboard_webos.c`: EXIT→open overlay, BACK→VK_ESCAPE). Needs `KEYS_EXIT` (above) or gesture SIGTERMs instead of delivering 505.
- **Gamepad disconnect shortcuts must be holds, not presses** (`runtime::input::DisconnectChord`, 2 s). Guide, both shoulders, or Start+Back opens in-stream disconnect dialog — and every one of those buttons is also forwarded as real game input, which is the whole constraint: L1+R1 in particular is a common in-game binding, so press-to-fire would kill streams mid-play. Chord state tracked from transitions (SDL reports no held-state here), **cleared when it fires or pad unplugs** — an open dialog swallows controller events and an unplugged pad sends no releases, so without that the buttons stay logically down and the dialog reopens the moment it's dismissed.
- **Hidden window gets no pointer input.** Keep it mapped and fully transparent `RGBA(0,0,0,0)` each frame so NDL plane shows through (not `.hide()`).
- **Two independent cursors** — webOS draws its own pointer, the host draws a second one over the network. Three levers, in the order they matter: `EVIOCGRAB` on the mouse's evdev node (starves the compositor of reports — the load-bearing one, trade-offs in the HID bullet below), `SDL_webOSCursorVisibility` → `wl_webos_input_manager.set_cursor_visibility`, and `show_cursor` for SDL's own cursor object.
  - **The compositor's repaint is lazy, in both directions.** `libWebOSCoreCompositor` branches on visibility: visible synthesizes a mouse event, invisible "let cursor be updated by upcoming event" — the pointer is only *marked*, and the next pointer event does the drawing. Under the grab no such event arrives, so an arrow already on screen survives the hide until something on an ungrabbed node (wheel, D-pad) flushes it; showing is equally stuck, leaving the desktop-mode arrow retracted until a button press. `Cursor::flush` supplies the event with a warp — to screen centre while captured, to the pointer's own position otherwise. This is why `set_cursor_visibility` read as "does nothing on webOS 26" for four attempts; it works, and the two timer-based workarounds built on that misreading (an unbounded 4 Hz re-assert, and one bounded to the seconds after a capture, chasing the arrow the HDR mode switch repaints) are gone.
  - **`WEBOS_CURSOR_TIMEOUT=0`** in surface-manager's environment, so the compositor's own inactivity auto-hide never fires. Nothing retracts the arrow on its own.
  - **Don't guard on `is_cursor_showing()`.** `SDL_ShowCursor` reaches the Wayland backend only when its cached `cursor_shown` flips, so a repeat hide is a silent no-op and the query reports "hidden" while the TV visibly draws an arrow.
  - Motion is **not** scaled. A client-side damping factor was a guess that only masked the jitter the evdev path below actually fixes.
- **Absolute pointer input is bounded by the panel; captured streams need relative.** webOS's pointer can't leave the screen, so `MouseMoveAbs` saturates at the edge — the host cursor can't reach a second display, and games wanting continuous motion stall. "Cursor capture" therefore also switches SDL to relative mode and sends `InputKind::MouseMove` deltas. webOS advertises no `zwp_pointer_constraints_v1`, so the SDL fork emulates relative mode by warping its own pointer to screen centre each motion (`wl_starfish_pointer_set_cursor_position`) — which is what makes the deltas unbounded. `SDL_SetRelativeMouseMode` therefore always returns 0 here, even where the standard protocols are absent. The protocol also carries a host-driven relative hint (`PunktfunkCursorState` flags bit 1) that would drive this automatically; the cursor channel is unconsumed so far.
- **A real HID mouse must bypass SDL — `/dev/input/event*` is readable from the jail.** Unlike hidraw (above), the evdev nodes are reachable: `root:compositor 0660`, and the app's uid carries gid 505 (`compositor`) in its supplementary groups — the same access the pad's `EV_FF` rumble node already relies on. Motion arriving via SDL comes from the compositor's pointer, smoothed and resampled for a wrist-waved remote, and jitters in games no matter what the client does with the deltas; `platform::webos::evdev` reads the mouse directly instead (aurora-tv's "Use Hardware Mouse"). Constraints, each learned the hard way:
  - **Keyboards are grabbed in both cursor modes, mice only under Capture.** An ungrabbed USB keyboard still reaches surface-manager, which reads modifier+click as a system gesture and warps its pointer to screen centre — with Capture off (CAD, desktop) the TV cursor and the host's then alternate between centre and the real mouse position on every Ctrl/Alt/Shift+click. Grabbing keyboards fixes it without costing the TV pointer, which desktop mode needs to aim.
  - **A grab is per node, never per event type**, so a combo keyboard+mouse node has its pointer forwarded too or the mouse goes dead.
  - **Keyboard nodes are `KEY_A`/`KEY_LEFTCTRL` minus a name denylist** (`LGE *`, `CHECK INPUT`, …): LG's virtual remotes advertise a full QWERTY keymap, and grabbing one leaves the TV unnavigable.
  - **SDL echo suppression differs by mode.** Capture on + HID mouse drops **all** SDL pointer events; Capture off drops only motion, and only within the keyboard's recency window (the compositor's warp-to-centre), since its clicks are the real ones.
  - **SDL relative mode must be off** — the fork warps its pointer per motion event, a thousand pointless compositor round-trips a second.
  - **Renice the reader thread to −10**, like the video pump. At nice 0 it lost the CPU to the boosted decode threads for up to 28 ms while a 1 kHz mouse kept reporting — jitter indistinguishable from the compositor's.
  - **Device filter is `EV_REL` with `REL_X`/`REL_Y` and *not* an absolute pointer** — test `ABS_X`/`ABS_Y` specifically, since real mice advertise stray absolute axes (the Logitech receiver here reports `ABS_VOLUME` for its media keys).
  - **`EVIOCGRAB` is scoped to `HidInput::set_active`, not held for the reader's life.** The kernel releases the grab the moment our fd closes (including on panic), so a wedged reader costs "no HID input", never a TV-wide dead mouse; surface-manager's own fd stays open and just stops receiving. The same flag gates whether the reader calls its `sink`, so "grabbed" and "forwarded to the host" can't drift apart the way two independent atomics could. `commons-evmouse` (moonlight-tv/aurora-tv's backing lib) ships the same idempotent-per-fd `EVIOCGRAB(1/0)` primitive but never calls it (its default open path forces `grab=false`) — this is a novel wiring of a proven primitive, not a ported fix.
  - **Hot-plug rescans are gated on `/dev/input`'s mtime.** Opening a node costs ~40 ms on this TV and ~20 nodes are empty (`ENXIO`), so an unconditional rescan stalls the reader for most of a second.
- **Color buttons: Green/Yellow/Blue need raw scancode polling, Red does not — Red has no usable scancode at all.** `SDL_SCANCODE_WEBOS_{RED..BLUE}=486..489` exist in the fork's `SDL_scancode.h` and `SDL_scancode_webos.c` maps IR keycodes 406..409 onto them, but only 487..489 ever appear in the keyboard-state array (`webos_scancode_down`). Red instead arrives like Back does: a plain `KeyDown` carrying **keycode 2097169** and `scancode: None` (confirmed on-device; `0x200000 + n`, same family as Back's 2097155). So poll the other three, match Red as a keycode. Nothing to unlock — there is no `ACCESS_POLICY_KEYS_*` hint for colour keys.
- **Don't toggle window show/hide while NDL composites.** Silently kills process (uncatchable Wayland crash). Test visibility changes in isolation.

## Runtime gotchas (LG CX/G5)

- Apps install to `/media/developer/apps/usr/palm/applications/<appid>/` = `$HOME` (writable dir for logs, `settings.json`, the art cache and the client identity PEMs).
- `luna-send` over raw ssh **needs `ssh -tt`** (real PTY) or output silently swallowed — the task
  targets go through `ares-install`/`ares-launch` instead, which don't have this problem.
- **Black screen despite decode**: launch through real app lifecycle (`luna-send .../launch`, SAM jailed uid). NDL punch-through only composites for SAM-managed foreground app.
- No env vars in SAM launch, but `params` in `applicationManager/launch` reaches native app as argv[1] JSON (parsed by `logger::launch`).
- SDL2/Wayland may report `refresh_rate=0` — clamp to sensible default.

## ChaCha20 over AES-GCM

CX/G5 are 32-bit userland on ARMv8-A. RustCrypto's `aes` crate has ARMv8 intrinsics for `aarch64` only; 32-bit ARM falls back to software regardless. ChaCha20 (add/rotate/xor, no crypto instructions) stays fast. Advertise `VIDEO_CAP_CHACHA20` unconditionally in `session::connect` — only cipher this client speaks.

## Large library handling

- **Tile windowing**: `app::render::prepare` builds tiles only for rows within `CARD_PREFETCH_ROWS` of viewport, at most `CARD_BUILD_BUDGET` (a time budget) per frame. Deliberately larger `CARD_KEEP_ROWS` (hysteresis stops oscillation).
- **Cover art**: `ArtLoader` request/response (UI asks for visible covers, forgets scrolled ones). Cached on disk as *encoded* bytes (`$HOME/art-cache/`, write-then-rename). Failed decodes deleted.
- Effect at 365 titles: retained tiles drop from ~366 to ~40; decoded covers from 365 to viewport window (~5 columns).

## Audio (software Opus path)

Two threads and a ring: `session::pump::audio_feed_pump` decodes Opus and posts chunks down a bounded channel; SDL's audio callback (`platform::webos::audio::RingCallback`) drains that into a ring it owns and serves the device from it under `punktfunk_core::audio::JitterPolicy` — the same de-jitter state machine every other punktfunk client ring runs.

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

## NDL's audio plane: why every load has one

⚠ **NDL only paces the picture when its audio plane is fed.** On a video-only load it ignores
`pauseAtDecodeTime` entirely and presents at feed cadence, which beats against a 120 Hz panel —
the long-standing "smooth at 1080p, randomly smooth above it" stutter. Measured on a CX: frames
stamped ~60 ms ahead of the player clock still left `render_buffer_length` at 0-1 and still
stuttered, and the same session with the audio plane fed was smooth.

So **every accepted V2 load asks for a stereo audio plane**, and what rides it is a separate
question:

- **PCM to the same plane** (Experimental → "Audio via TV sink", opt-in) — `session::pump::ndl_pcm_audio_pump`
  decodes Opus here and feeds S16LE, stamped on the video timeline. See "Latency levers" below.
- **Hardware Opus decode** (Experimental → "Audio offload", opt-in) — `session::pump::ndl_audio_pump`
  feeds the real stream, stamped on the video timeline; no SDL device is opened.
- **The clock plane** (default, and the only option for 5.1/7.1) — `NdlVideo::run_clock_plane`
  feeds a silent Opus metronome stamped in NDL's own player-clock domain, while
  `platform::webos::audio` decodes the real audio to SDL as before. Confirmed at 4K120 5.1.

`run_clock_plane` runs on **both** routes (`session::connect::spawn_plane_threads`): under offload
it yields to the real stream and only fills in after 300 ms with no packet, since a host that stops
sending would otherwise starve the plane and freeze the picture.

A set that refuses the audio-enabled load falls back to video-only inside `NdlVideo::load` and
gives up pacing with it; the session log names which of the routes it took. **NDL v1 and SMP have
no Opus audio type at all**, so webOS 4 has no pacing reference — the lever there would be gating
frame release at feed time, which is not built.

Blind alleys, measured and ruled out, so they are not re-tried: NDL's standing cushion depth
(`render_buffer_length` is 0-1 in smooth AND stuttery sessions), feed lateness (45-60 ms of slack
either way), software Opus decode cost (5% of a core, `dropped=0`), and HDR mode re-entry (a real bug,
fixed in *Video decode* above, but not this one).

### Wiring the plane

Byte-exact with `mariotaku/ss4s` `ndl/webos5`: `NdlAudioConfig.sample_rate` in **kHz** (`48.0`,
not `48000.0`), the stereo `opus_empty_frame_211 = {0xec,0xff,0xfe}` decoder prime, and combined
audio+video in one load. Struct layouts (`NDL_DIRECTMEDIA_AUDIO_OPUS_INFO_T`, `..._DATA_INFO_T`)
match `webosbrew/webos-userland` field-for-field, including the explicit trailing `_padding` — the
whole struct is memcpy'd into a fixed-size union arm, so **any implicit padding in a `repr(C)`
struct handed to NDL is uninitialized stack on the wire**.

⚠ **The prime is what completes the load.** An audio-enabled load does not report `LOADCOMPLETED`
until its audio plane has received a packet — but the pumps that would send one don't spawn until
`session::connect` returns, which is after the load wait. That deadlock read as a whole session of
black picture with working sound, no error anywhere. `NdlVideo::prime_audio` feeds bursts of empty
frames through the load window itself; on a CX that turns "never" into `LOADCOMPLETED` in ~40 ms.
The prime's highest stamp seeds `last_audio_pts_ms`, so the first real packets are floored rather
than read as a rewind. The corollary: **`LOADCOMPLETED` is reliable** and can gate things — the
earlier claim that it "lands anywhere from 4s to never" was this bug.

⚠ **Never flush a pipeline that has not finished loading — it kills audio for the session.**
`NDL_DirectVideoFlushRenderBuffer` before `LOADCOMPLETED` takes the audio plane out permanently;
video recovers and gives no sign. `ensure_loaded` returns a typed `ndl::NotLoadedYet` and
`sink::submit` holds + requests a keyframe **without** flushing. Nothing is queued at that point
anyway, so the flush was never doing work. `NDL_DirectAudioPlay` returns 0 either way — there is
no error to find on the audio side, so do not go looking for one.

⚠ **Audio stamps must never go backwards — NDL reads a rewind as a mute for the rest of the
session**, and does not resync. `NdlVideo::play_audio` and `NdlVideo::burst_silence` are the only
feed points, both serialised under `lock_ffi` and both flooring at `last_audio_pts_ms` — the floor
must be read under that guard, or a packet measured against an older ceiling blocks on the lock and
then hands NDL the stale stamp. For hardware decode the offset is additionally
**latched, not recomputed per frame**: a receive-backlog flush jumps host PTS forward while the
player clock does not, and a re-derived offset would drag the audio stamp backwards by the size of
the jump. `latch_pts_offset` takes only the first value after each `clear_pts_offset` (hold
resume, pacing off→on edge), and only off a frame NDL **accepted** — `play_audio` has no
`ensure_loaded` guard of its own, so the latch doubles as the audio thread's start gate.

⚠ **But flooring every packet on that ceiling is itself a mute.** A hold resume re-anchors the
video plane onto the *current player clock*; when that maps the resumed audio BELOW the ceiling —
the video plane was running ahead, or the clock plane bursted its `PRIME_LEAD` of silence during
the gap — `fetch_max` pins every packet to one stamp, audio stops advancing, and nothing lifts it
off again for the rest of the session. Field case (CX, 2026-08-20, offload on): the startup ABR
probe at 300 Mbps saturated the airlink, one freeze-until-reanchor hold followed, and audio was
gone from that point while video recovered normally. The fix is a per-latch `audio_skew_ms`: the
first real packet after a re-latch shifts the whole stream to one packet above the ceiling and
advances by its own cadence from there, leaving the floor as a defensive no-op. The drop path
(`play_audio` with no latched offset) also **logs the gap on the packet that ends it** — silent, it
cost a session its audio with nothing in the log to find.

⚠ The audio-enabled load returns success even on a TV that then plays nothing, so **no runtime
probe can distinguish the two**. If a model regresses, the `NDL load state:`
(`LOADCOMPLETED`/`PLAYING`) log says whether the pipeline ever started, and turning Experimental →
"Audio offload" off is the way out of the hardware-decode half.

## Latency levers on the NDL path (2026-08-21, all three UNMEASURED on hardware)

The video feed itself is already copy-free — core reassembles one contiguous `Vec` (which
`NDL_DirectVideoPlay` requires) and `sink::submit` passes that pointer straight through, no
Annex-B rewrite, no client-side queue. So these three are about *when* bytes are released, not
how they move.

**1. The PTS anchor's standing lead (`session::timeline`, on by default).** `HostPtsAnchor` maps
`base = player0 + (host_pts - host0)`, which bakes frame 0's own delivery latency into every later
frame: one that arrives faster than frame 0 did gets a stamp in NDL's future and
`pauseAtDecodeTime` holds it there for the difference. Frame 0 is the first keyframe, behind the
handshake and (on Automatic) the ABR capacity probe — likely the session's worst frame. SS4S
sidesteps this by stamping arrival time (`now - mediaLoadedTime`), which never holds but also
discards the spacing NDL paces on. The trim keeps the spacing: the MINIMUM lead over a 500 ms
window is slack no frame in that window needed, so it is subtracted, ramped at a quarter frame
interval per frame (the stamps must never go backwards) and only inside the first 3 s after an
anchor. `pts lead: trimming Xms` at INFO, plus `pts_trim=` on the video heartbeat, is how much
standing hold the session started with — a number not otherwise observable from the app.

⚠ **On the software route the trim moves the picture, not the sound.** Video reaches the glass
`trim` ms earlier and the SDL ring is untouched, so audio ends up that much LATE — and `AvSync` is
still measure-only, so nothing corrects it. The `A/V` figure on the stats overlay moves by the same
amount, which is the check: if a trim of 30 ms shows up as 30 ms of fresh audio lateness, arming
the sync loop (§ "A/V sync") is the follow-up, not backing the trim out.

⚠ **Armed only where NDL's plane carries the metronome** (`SinkConfig::trim_pts_lead`, false on
both NDL-plane audio routes). Audio there is stamped through this mapping's offset and
`play_audio` can only move a stamp forward — a rewind mutes the session for good — so a video
timeline pulled earlier would land as exactly that much lip-sync error instead.

**2. Slice-progressive feed (Experimental → "Progressive feed", off).** Without it the decoder
sees byte 0 of a frame only once that frame's LAST datagram lands; at 200 Mbps a keyframe is many
datagrams and the tail of that reassembly wait is pure latency. Core has had the plumbing all
along (`Frame::part`, the `frame_parts` connect flag) and this client passed `false`.
`session::pump`'s `AuParts` implements core's contract — parts in order, an `offset` mismatch or a
new `first` over an open AU means that AU died — and reports the break as loss, which is what puts
the sink into freeze-until-reanchor and asks for a keyframe. `session::sink` skips the per-frame
reference points (`video_e2e`, the decode report, the audio latch) on a piece that is not the AU's
last, since a piece is not a presentable frame.

⚠ **NDL has no `PARTIAL_FRAME` flag and no AU-boundary flag at all** — it takes raw Annex-B and
must be finding boundaries by start code, which is the whole reason to expect a fragmented feed to
work, and the whole reason it might not. Clamped to NDL v2 (v1's feed carries no timestamp to
repeat across pieces; SMP's load shape is fragile enough already). Failure mode is visible
corruption; the way out is the toggle.

**3. PCM on NDL's audio plane (Experimental → "Audio via TV sink", off).** ss4s
(`webos5/ndl_audio.c`) *prefers* `NDL_AUDIO_TYPE_PCM` over Opus for stereo, and the struct is in
`webos-userland`: `NDL_DIRECTAUDIO_PCM_INFO_T { type, unknown1, format, layout, channelMode,
sampleRate }` — 24 of the union's 32 bytes on this ABI, `format` = `"S16LE"`, `sampleRate` = the
enum's `48KHZ` (1), NOT hertz, and `layout` left **null** exactly as ss4s leaves it. So the
software decoder can keep running (concealment, layouts, every core fix) while its samples go to
the TV's own sink instead of SDL's: the 25-90 ms jitter ring and the 512-frame device buffer leave
the path, and audio lands on the same hardware clock as the picture — which is also what would
make the A/V offset measurable rather than the estimate § "A/V sync" describes.

- The plane's silence is format-specific now (`NdlAudioConfig::silence`): the load prime and
  `run_clock_plane` feed 5 ms of zeroed S16LE instead of `opus_empty_frame_211`. A PCM plane fed
  an Opus frame would read it as 3 bytes of samples.
- 5.1 needs webOS 7's multi-channel PCM sink, probed the way ss4s probes it — the presence of
  `NDL_DirectAudioRegisterCallback`. Without it the route falls back to software rather than
  loading a plane for a width the set can't take (a failed load costs the picture its pacing).
- `NDL_DirectAudioRegisterCallback` itself is the pull-based feed, i.e. real hardware pacing. Not
  used; it is the next step if this route proves out.
- Unknowns, in order: the depth NDL holds on that plane (it is not `render_buffer_length` and
  there is no query), and whether `Settings::ndl_audio_pcm` beats plain Opus offload where offload
  works at all. The three routes are named on the overlay (`Opus SW` / `Opus HW` / `PCM HW`) and
  in the `audio path:` log line precisely so a report says which one produced the numbers.

## ABR startup probe: 2 Gbps, upstream-hardcoded

**"Automatic" bitrate fires a 2 Gbps burst ~2 s into every session, and on Wi-Fi that can cost the session its video entirely** — not a slow start but a flow that never establishes. Measured on G5: a "successful" probe still reported `send_dropped=20211`, i.e. link hammered far past what it can carry (~245 Mbps airlink ceiling), and probes that get nothing back sit on core's 6 s timeout. Capped at 300 Mbps the same link reports `send_dropped=0-167` and stream starts are reliable.

Don't read a slow *start* as this bug — a host compositor coming up has its own startup time, and video legitimately arrives late on first connect of a session. Signal that matters: packet drops on the probe and video that never arrives at all.

This is `CAPACITY_PROBE_KBPS` in `punktfunk-core`'s `client/pump/data.rs` — a **hardcoded const with no cap knob** — directly at odds with this client capping its own speed test for the same "unbounded firehose starves the app" reason (below).

**Fixed by capping the burst**: `main.rs`'s `set_abr_env` sets `PUNKTFUNK_ABR_PROBE_KBPS` before anything spawns a thread (`setenv` isn't thread-safe, and core reads it while building its data-plane pump). Same order as the speed test's own cap, still above the airlink ceiling below — measures the link without knocking it over. Knob is core-side; **core v0.22.3 is the first release carrying it**, which is why the pin moved off v0.21.0. An older core ignores the variable.

**300 was still too high on the CX** (2026-08-20): the probe saturated the airlink and opened a freeze-until-reanchor hold seconds into the session — the "Connection issues — recovering" toast — which on the offload path then cost the session its audio (see *NDL's audio plane*). Both knobs are now derived from `core::model::BITRATE_MAX_KBPS` — the settings slider's own ceiling, 200 Mbps, so there is one number to change: `PUNKTFUNK_ABR_MAX_MBPS` clamps the climb ceiling however it is learned (`abr::ceiling_cap_from_env`; the probe MEASURES that ceiling and `set_ceiling` is monotonic, so an over-read is otherwise permanent for the session), and the probe burst matches it — bursting above a ceiling the session can never use only buys loss. Descending below 200 stays core's job; its congestion signals walk the rate to their own 5 Mbps floor and **there is no client-side API to lower a ceiling mid-session**.

That bump also brought a `connect` signature change — a `name: Option<String>` (label the host's pending-approval list shows) between `launch` and `pin`. All four call sites pass `None`, preserving fingerprint-derived label; sending a real TV name is a separate user-visible change.

Blind alleys, so they aren't re-tried:

- `bitrate_kbps == 0` (Automatic) arms **both** the AIMD controller and this probe — client cannot separate them.
- `PUNKTFUNK_ABR_PROBE=0` disables the probe but leaves climb ceiling at negotiated start rate (~20 Mbps), which core's own comment calls a box "Automatic could NEVER climb out of".
- Running our own capped probe instead does **not** work: `request_probe` completes, but `abr.set_ceiling` is only called from core's own probe path (gated on its `capacity_probe_deadline`), so ceiling never moves. No public bitrate/ceiling setter on `NativeClient`.
- Pinning a fixed bitrate also disarms the probe, but costs mid-session adaptation entirely.

## Network speed test quirks

Burst is 320 Mbps / 3 s (not 3 Gbps / 5 s) — the UI thread shares a 3-core Cortex-A9, and an unbounded firehose starves the app. 320 still detects any ceiling that would change the clamped recommendation (>~285 Mbps). Probe must advertise `VIDEO_CAP_CHACHA20` like a real session (core's `bytes_received` increments *after* AEAD decrypt). **~245 Mbps airlink ceiling** measured on G5 Wi-Fi (MediaTek USB 2.0 Hi-Speed bus), nothing client code can raise. New flows sometimes black-hole 10-29 s (AP/driver setup), so `session::probe::run_speed_probe` waits for the first completed video frame (cap 35 s) before bursting — plane live, path warm.

## Video backends: NDL (default) + SMP on webOS <5

NDL DirectMedia is the only backend on webOS 5+. NDL has no decode context; calls go through `NdlVideo::ffi` mutex (header says not thread-safe). AV1 remains disabled (never produced picture).

SMP (`libplayerAPIs_C.so`, loaded via `dlopen`) is only for webOS 3.5-4.x, where it is the only HEVC/HDR path. Choosing SMP widens `core::caps`; SMP load failure falls back to NDL v1.

Critical SMP constraints:

- **Sink wiring is version-specific** (`smp/sink.rs`):
  - webOS 5+: SDL exported window id in load payload.
  - webOS 3.5-4.x: ACB (`libAcbAPI.so`) path, no window. Required sequence: `initialize(PLAYER_TYPE_MSE=10)`, `setMediaId(getMediaID())` pre-load, `setSinkType(MAIN)` + `setState(LOADED)` on LOADCOMPLETED, `setDisplayWindow`, then `setState(PLAYING)` on first accepted frame, and `setMediaVideoData` with `STR_VIDEO_INFO` (+`hdrType` for HDR).
  - Wrong shape: load succeeds, frames accepted, nothing composited.
- **Feed PTS must be SMP-relative** (`now - openTime` ns), not host clock. `session::sink` maps host PTS through `HostPtsAnchor` (same model as NDL v2).
- **Load payload is fragile:** only one known shape works; trimmed variants never completed load.

Still unverified on real 3.5-4.x hardware. webOS 3.5 may require `playerAPIs_C_Legacy`; `c_shim.cpp` currently targets the current SDK header.

## NDL generations: v2 (webOS 5+) and v1 (3.5-4.x)

- Same library, two ABIs: v2 (`DirectMediaLoad`, `DirectVideoPlay`, `FlushRenderBuffer`, `GetRenderBufferLength`, `SetHDRInfo`) vs v1 (`DirectVideoOpen/SetCallback/SetArea/PlayWithCallback/Close`). webOS 4 has no v2 symbols.
- **Must `dlopen`, never link** `libNDL_directmedia.so.1`: a `DT_NEEDED` breaks webOS 4 startup under BIND_NOW (fails before `main`). Do not re-add `#[link(name = "NDL_directmedia")]`; `-Wl,-z,lazy` is not an acceptable workaround.
- Backend generation comes from `device::ndl_generation()` (`sdkVersion`): v1 for `<5`, v2 for `>=5` and unknown. Version selects what to try; `dlsym` is final authority (no silent fallback).
- v1 limits: H.264 + SDR/BT.709 only; no input PTS, render-buffer query, flush, or HDR API. **Resolution is not capped here**: decode dimensions are passed through; `1920x1080` in `ndl/v1.rs` is only the display rect from `SetArea`.
- `NDL_DIRECTVIDEO_DATA_INFO_T` must include `source` (`width,height,source`). Omitting it fed stack garbage into `NDL_DirectVideoOpen`; now explicitly `NONE` (0) (fixed 2026-08-12).
- M3/KADP runtime codec patch remains intentionally unused.
