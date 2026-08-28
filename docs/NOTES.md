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

## Audio: two routes, one pipeline (SDL is the default)

`Settings` → **Experimental** → **Audio processing** picks the route
(`core::model::AudioRoutePref`), and both are
built on the same pipeline: `session::audio::AudioStage` decodes (or forwards) into whatever
`core::media::AudioSink` the route selected, and one pump drives it. Adding a third route is one
`AudioSink` impl.

| Route | Label | Path | Layouts |
| --- | --- | --- | --- |
| `Software` (default) | Software (SDL) | libopus here → SDL device, NDL's clock plane on its metronome | up to 7.1 |
| `NdlOpus` | Offload (NDL) | the wire's Opus, decoded by the TV | 2 |

**Why software is the default.** NDL paces the picture against a *fed* audio plane, so a plane fed
from the network inherits the stream's arrival jitter — which is the stutter the silent clock plane
was introduced to cure. The offload route is shorter and stays selectable for exactly that
comparison; the overlay names which one ran (`Opus SW` / `Opus HW`).

**The offload route is stereo, and stereo only.** NDL's Opus struct has no multistream mapping
field, so there is no 5.1 to negotiate — and some sets accept the load and then play nothing, which
no runtime probe detects. That is why it lives under Experimental rather than beside the codec pick.

**Offload is not free of the metronome.** `run_clock_plane` still runs on that route, yielding to
the real stream and filling silence only after `REAL_FEED_GRACE_MS` without a packet — a dead host
capture would otherwise starve the plane and freeze the picture.

**The offload route exists only under NDL v2.** v1 (webOS 4 and below) has no audio type at all,
so `caps::VideoCaps::audio_plane` is false there and
`AudioRoutePref::available` collapses to `Software` — the row locks, and `Settings::clamp_to_caps`
rewrites a document carried over from a v2 set. The Audio row's layouts follow the *selected*
route, so picking `Offload (NDL)` locks that row to stereo with the reason on it.

**Nothing is ever mixed down, and the layout row is a preference.** `Settings::audio_channels` says
"5.1 where it can play"; `Negotiated::clamp` is the one place it becomes a width on the wire, narrowing it by
what the selected route carries (`AudioRoutePref::max_channels`) and by what the TV's Sound Out
passes right now (`ndl::audio_output_width`). So a layout the sink can't put on a speaker is never
encoded, never sent, never decoded and never folded — and the preference survives a route change or
an unplugged receiver instead of being rewritten out of the document. A width mismatch at
`AudioStage::new` is an error, not a downmix.

**The menu is narrowed by the static limits only.** The Audio row lists what this client can
decode, capped by what the *selected* route can put on a speaker — the Opus plane carries nothing
above stereo, so those widths are never offered, and a route left with one entry locks the row with
the reason on it. The TV's Sound Out is deliberately not in that filter: it changes under a running
app, so it applies per session and lands in the log, not in a menu that would be stale by the time
it was drawn. The stored `audio_channels` is still never rewritten (`menu::audio_row_channels` shows
the preference held down to the route), so a 5.1 pick comes back whole on the route that plays it.

- **Capability and routing are different questions, asked in different places.**
  `NDL_DirectAudioSupportMultiChannel` answers the second: whether 5.1 reaches a speaker *right
  now*, which also depends on Sound Out (TV speakers are 2.0/2.2 and ARC/optical carry 2-channel
  PCM only). ss4s declines to check it at all and lets webOS fold. Here `ndl::audio_output_width`
  reads it **once per session, at connect** — fresh, after `NDL_DirectMediaInit`, and early enough
  to size the wire request. Never in the menu: the answer would be stale by the time it was drawn.
  It initialises NDL a moment before the load would have anyway (process-global and idempotent),
  so it costs no extra call. It narrows the SOFTWARE route too: 5.1 the TV would only fold down is
  airlink, host CPU and local decode spent on nothing.
- ⚠ **`NDL_DirectAudioSupportMultiChannel` has an out-parameter**:
  `int NDL_DirectAudioSupportMultiChannel(int *isSupported)`, returning 0/-1, with the code written
  through the pointer — `0` unsupported, `1` no device, `2` device but not passthrough, `3` will
  play. Reading the *return* as the code (as this client did) is both wrong and UB on ARM EABI: the
  callee writes through whatever `r0` held. The `NDLMultiChannelPCMCallback` codes documented beside
  it are the same ladder shifted down by one; they are not interchangeable.
- ⚠ **Optional NDL symbols must be probed after the library is open.** `RTLD_DEFAULT` finds nothing
  until something has `dlopen`'d `libNDL_directmedia` — and the capability probes run at startup,
  before any decode session. `ffi::optional_sym` forces `ffi::common()` first; without it every
  optional symbol reads as absent and the TV silently loses 5.1.
- **Samples are never converted.** libopus decodes straight into f32, which is exactly what the SDL
  device takes; the offload route decodes nothing at all. There is no second buffer and no
  conversion pass on either route.
- **The software route's latency is buffering, not decode.** Software Opus is 5% of a core and the
  target is hardware-FP (`-soft-float`, § "Toolchain"), so the only client-side terms are the ring
  depth and the device quantum. Two things follow, and they are the whole lever list here:
  - **The prime overshoots, and the shed is what takes it back.** The ring is inspected once per
    callback, so it crosses the target somewhere inside a 10.67 ms period in 5 ms steps — first
    serve is target+0..15 ms. An earlier revision drained that excess at the prime edge, which is
    free by construction but only handles the prime; `JitterPolicy`'s drift shed handles it and
    every later source of the same drift (host capture clock vs. this DAC) by walking the depth
    back to target one crossfaded 5 ms frame at a time. Letting the policy own priming outright is
    the cheaper correctness call than second-guessing its state machine for one transient.
  - **The device quantum is logged at open** (`SDL audio device:`). SDL may negotiate something
    other than the requested 512 frames, and a larger one silently raises the policy's effective
    target, which is floored at `one callback + 5 ms`. Read that line before concluding anything
    about this route's latency; without it there is no way to tell from a log where it went.
- **The SDL ring runs `punktfunk_core::audio::JitterPolicy`** (`JitterTuning::AAUDIO`, unmodified),
  the same de-jitter state machine the Linux, Windows, Android and Apple rings use. Prime to an
  adaptive target, grow it only on a set that actually underruns, and walk drift back down one
  crossfaded 5 ms frame at a time. `crossfade_drop` fades BOTH corrections — the smooth shed and the
  hard-cap trim.
  - ⚠ **This was removed once on `ndl-latency-levers` and restored deliberately.** The removal was
    credited with "~35 ms of floor", and that was wrong: the old local preset was
    `base_target_ms: 25` / `max_target_ms: 90` and the fixed prime that replaced it was also 25/90,
    so no floor was ever saved. What was actually lost was jitter resilience — the adaptive floor,
    and the crossfaded shed (replaced by an uncrossfaded drop of up to 65 ms, i.e. an audible
    click) — on the fleet's worst link. Do not delete it again without numbers from
    `audio playback (SDL device)`.
  - **The preset is `AAUDIO` rather than a local copy.** Field for field it already *is* what the
    local `WEBOS_TUNING` was, with the old `deprime_after: 5` **callbacks** now expressed as
    `deprime_ms: 60` — core moved that fuse to time because a callback count means a different
    span on every device. AAudio's rationale (raw callback, client owns the buffer, Wi-Fi
    power-save bunching arrives as underruns) is this TV's situation exactly.
  - **The A/V sync loop is not wired.** `set_sync_target` is never called, which core documents as
    reproducing unsynchronised behaviour exactly. It never steered here anyway — it was gated
    behind an `$HOME/av-trim-ms.conf` nobody measured, and the video reference this platform can
    build is biased low by NDL's unobservable decode+panel term. See § "A/V sync".
  - **Read `target_ms` in the debug line.** It is the adaptive floor's current answer, and the one
    figure that says whether this set needed more than the 25 ms base. `sheds` vs `trims` separates
    "drift corrected inaudibly" from "the link outran the headroom".
  - **The device quantum is logged at open** (`SDL audio device:`). SDL may negotiate something
    other than the requested 512 frames, and a larger one silently raises the policy's effective
    target, which is floored at `one callback + 5 ms`.

**Blind alleys, so they aren't re-tried:**
- ⚠ **The NDL PCM plane was built, measured and removed.** A third route decoded Opus here and fed
  NDL's `NDL_AUDIO_TYPE_PCM` plane. Fed on arrival it made the plane's depth — the thing NDL paces
  the PICTURE on — a function of network jitter, and the field report was intermittent lag. A paced
  ring in front of it (a feeder thread topping the plane up to a standing lead on a fixed cadence)
  fixed that, and what was left was a **small** latency win over SDL for a route that could never
  carry 7.1, whose `"6-channel"` interleave order was inferred from ss4s and never verified on a
  set, and whose stamps came off the plane's own clock rather than a host PTS. Not worth a third
  hardware path: for stereo the offload route is shorter still, and for anything wider software is
  the only route that plays it. Deleted along with `session::paced`, `ffi::AudioPcmInfo`,
  `NDL_51_ORDER`, `AudioFormat::PcmS16`, `NdlVideo::burst_pcm` and
  `ndl::audio_plane_max_channels`; see git history on `ndl-latency-levers` if it is ever revisited.
- **`sdl2::audio::AudioQueue` cannot carry a de-jitter policy** — `queue_audio`/`size`/`clear` and
  nothing else, no partial drop. That is why the pull callback stayed.
- **Do not put the audio drain back on the main loop.** It was there because `AudioQueue` is
  `!Send`, which put the audio cadence behind the UI's software rasterizer.
- **Do not shrink `DEVICE_BUFFER_FRAMES` below 512** to chase latency: a smaller quantum on this
  SoC buys more wakeups and more missed callbacks.
- **Do not split `lock_ffi` per plane** without device evidence. No NDL entry point is documented
  as thread-safe; the contention between the video feed and the audio bursts is real but a second
  guard is a guess about vendor internals, which is the class of change this file exists to warn
  against.
- **Do not fold the clock plane's keep-alive into the audio pump.** Its cadence is 20 ms and the
  pump parks up to 100 ms on an empty transport; one thread would mean a starved plane, i.e. the
  stutter the plane exists to prevent.

## A/V sync

The host stamps `pts_ns` on every audio datagram. With audio on NDL's plane **NDL does the
synchronisation**: both planes are stamped in one timeline (`NdlVideo::play_audio` maps host PTS
through the video plane's latched offset), and the client-side `AvSync` estimator went with the
jitter ring it existed to steer — there is no ring depth left to move.

What still matters:

- NDL is submit-only (`NDL_DirectVideoPlay` reports nothing about presentation), so glass time can
  only be estimated and the decode+panel constant after the render queue drains is not observable
  from the app at all. `session::sink::video_e2e_ns` still publishes that estimate for core.
- ⚠ **Use `frame.pts_ns`, never the paced value**, wherever a host-clock comparison is made. Both
  are in scope at the submit site with near-identical names; the paced one has been mapped into
  NDL's player clock by `session::timeline::Pacing`.
- ⚠ **NDL can fail the whole load asynchronously, and then never recovers.** Seen on a CX: load
  state `0x12` with `errorCode 600`, after which every `NDL_DirectVideoPlay` returns -1, the clock
  plane's `NDL_DirectAudioPlay` fails too (so the thread that paces the picture exits for good) and
  NDL reports `UNLOADCOMPLETED` on its own. There is no reload path in-session, and a re-anchor —
  the response to lost frames — does nothing for a lost pipeline, so the client spun on failed feeds
  with a frozen picture and no audio while the QUIC session stayed perfectly healthy. `ndl::fatal()`
  latches that state, `VideoSink::is_dead` carries it up backend-blind, and the stream loop ends the
  session and returns to the menu. What PROVOKES the 600 is still unknown.
- The estimator's unit tests ship in-tree but **cannot run off-device**: `cargo test` links the
  whole binary and `-lNDL_directmedia` exists only in the cross sysroot.

## NDL's audio plane: why every load has one

⚠ **NDL only paces the picture when its audio plane is fed.** On a video-only load it ignores
`pauseAtDecodeTime` entirely and presents at feed cadence, which beats against a 120 Hz panel —
the long-standing "smooth at 1080p, randomly smooth above it" stutter. Measured on a CX: frames
stamped ~60 ms ahead of the player clock still left `render_buffer_length` at 0-1 and still
stuttered, and the same session with the audio plane fed was smooth.

So **every accepted V2 load asks for a stereo audio plane**, and what rides it is a separate
question:

- **The clock plane** (the default) — `NdlVideo::run_clock_plane` feeds a silent Opus metronome
  stamped in NDL's own player-clock domain, while `platform::webos::audio` decodes the real audio
  to SDL. Confirmed at 4K120 5.1.
- **Hardware Opus decode** (Experimental → "Audio processing" → Offload, opt-in) — the audio pump
  feeds the real stream, stamped on the video timeline; no SDL device is opened.

`run_clock_plane` runs on **both** routes (`session::pipeline::spawn_plane_threads`): under offload
it yields to the real stream and only fills in after 300 ms with no packet, since a host that stops
sending would otherwise starve the plane and freeze the picture.

A set that refuses the audio-enabled load falls back to video-only inside `NdlVideo::load` and
gives up pacing with it; the session log names which of the routes it took. **NDL v1 has
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
then hands NDL the stale stamp.

⚠ **The audio plane stamps off the PLAYER clock, not the host's (2026-08-28).** It used to map the
host capture PTS through the shared `SessionClock` the video plane published, with a per-latch
`audio_skew_ms` lifting each resumed run above the ceiling. That **ratchets**: a
freeze-until-reanchor stalls the mapped timeline while packets keep arriving, the resumed run lands
below the ceiling it already reached, and the only monotonic repair is to add lead — which nothing
in the session can ever pay back. Field case (CX, 2026-08-28, offload on): five re-anchors inside
four seconds walked the plane from 78 ms to 124 ms of lead and the audio was gone for the rest of
the session.

So audio is now stamped `player_clock + PLANE_LEAD_MS` and the host PTS is ignored
(`AudioSink::feed` takes it and drops it). A wall clock cannot ratchet: it advances at the same rate
whatever the host PTS does across a freeze, so a resumed run lands where an uninterrupted one would
have, and `last_audio_pts_ms` is left with nothing to do but absorb reordering. The clock plane
targets the same figure, so the two feeders share the ceiling without either driving it.
`load_confirmed` is the plane's start gate, which is what the video plane's latch used to double as.

⚠ **The ratchet was real and was NOT the mute.** Measured after the change, under a deliberately
saturated airlink (276 Mb/s of competing download against a 188 Mb/s stream): 32 re-anchors,
`plane_lead` pinned at 37-40 ms the whole way, `depth` flat at 40 ms, stamps provably monotonic and
evenly spaced — and the audio still died permanently. **Nothing the client hands NDL explains this
failure.** Do not spend another round on stamp arithmetic.

⚠ **The loss hold no longer flushes, and THIS is what was muting the plane (2026-08-28,
confirmed on device).** It was the last structural difference from `ss4s`, which never flushes
mid-stream — its only recovery is unload+load, and it does not lose its Opus plane. Every flush
stops the pipeline: each one used to be followed by `NDL load state: PLAYING (0x1a)`, a transition
NDL only makes from not-playing. Confirmation (CX, 276 Mb/s of competing download against a
188 Mb/s stream): 16 re-anchors, holds up to 2 s, **not one `PLAYING` transition in the whole log**,
`plane_lead` 38-40 ms flat, and audio intact — where the identical storm against the flushing build
killed it permanently. `NDL_DirectVideoFlushRenderBuffer` is safe to call and reports success; what
it costs you is the audio plane, silently, for the rest of the session. The decode-error
path still flushes, where the pipeline has actually errored and discarding its queue is the
documented response; loss is a network event and NDL's queue holds good frames the hold is about to
present anyway. This reopens a call the 2026-08-27 handover had marked a dead end — that verdict
rested on records scoped to *before* `LOADCOMPLETED`, and on `Pacing::reset` needing
`last_base_ns = 0`, which was only true *because* of the flush. `last_base_ns` now survives the
reset: without a flush the pipeline still holds everything fed before it, and a run restarting from
0 would walk the video stamp backwards.

Removed with the host-PTS mapping: `SessionClock` (its only reader was audio),
`AudioPlane::attach_clock`, `audio_skew_ms`, `skew_epoch`, the floored-packet run detector, and
`CadencePacer::ready_for_audio` / `note_audio_latched` with the convergence gate behind them.

This is where `mariotaku/ss4s` ended up too, from the other direction: `734e643` added a thread
feeding empty Opus frames through gaps ("if a huge gap appeared between frames, audio output will be
distorted"), then `ef0c0ae` deleted the whole mechanism and moved both planes onto
`CLOCK_MONOTONIC - mediaLoadedTime`. moonlight-tv#493 ("Stream loses audio after network hiccup") is
the unfixed version of this failure — same symptom, Opus route only, PCM never reproduces it, only a
full restart recovers.

Removed with it: `SessionClock` (its only reader was audio), `AudioPlane::attach_clock`,
`audio_skew_ms`, `skew_epoch`, the floored-packet run detector, and `CadencePacer::ready_for_audio`
/ `note_audio_latched` with the convergence gate behind them.

⚠ The audio-enabled load returns success even on a TV that then plays nothing, so **no runtime
probe can distinguish the two**. If a model regresses, the `NDL load state:`
(`LOADCOMPLETED`/`PLAYING`) log says whether the pipeline ever started, and turning Experimental →
"Audio offload" off is the way out of the hardware-decode half.

## Latency levers on the NDL path (2026-08-21; only the PTS trim has on-device numbers)

The video feed itself is already copy-free — core reassembles one contiguous `Vec` (which
`NDL_DirectVideoPlay` requires) and `sink::submit` passes that pointer straight through, no
Annex-B rewrite, no client-side queue. So these are about *when* bytes are released, not how they
move. (A third lever, PCM on NDL's audio plane, was built and then removed — see § "Audio".)

**1. The PTS anchor's standing lead (`session::timeline`, REMOVED 2026-08-27 — see lever 3, which
replaced it; kept here because the failure it describes is what the cadence loop has to keep
solving).** The anchor mapped
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

⚠ **The audio plane no longer latches this mapping at all (2026-08-28).** It used to: audio stamps
rode the offset the video plane published, so the plane could not latch a mapping that was still
moving, and a convergence gate (`CadencePacer::ready_for_audio`) held it while the estimate settled
— dropping real packets, 5 ms of audible silence each, through that window. Audio is now on the
player clock and has no mapping to wait for, so the gate and its silence are gone. See § "NDL's
audio plane" for why.

**2. Slice-progressive feed (on, every NDL v2 session).** Without it the decoder
sees byte 0 of a frame only once that frame's LAST datagram lands; at 200 Mbps a keyframe is many
datagrams and the tail of that reassembly wait is pure latency. Core has had the plumbing all
along (`Frame::part`, the `frame_parts` connect flag) and this client passed `false` until now.
`session::pump`'s `AuParts` implements core's contract — parts in order, an `offset` mismatch or a
new `first` over an open AU means that AU died — and reports the break as loss, which is what puts
the sink into freeze-until-reanchor and asks for a keyframe. `session::sink` skips the per-frame
reference points (`video_e2e`, the decode report, the audio latch) on a piece that is not the AU's
last, since a piece is not a presentable frame.

⚠ **NDL has no `PARTIAL_FRAME` flag and no AU-boundary flag at all** — it takes raw Annex-B and
must be finding boundaries by start code, which is the whole reason to expect a fragmented feed to
work, and the whole reason it might not. Clamped to NDL v2 (v1's feed carries no timestamp to
repeat across pieces). Failure mode is visible
corruption plus `frame parts:` warnings — there is no toggle, so a regression here means reverting
`Negotiated::clamp`'s `frame_parts`.

⚠ **Real audio on the plane must carry a lead, or the PICTURE stutters** (2026-08-21).
`run_clock_plane` held the plane `PRIME_LEAD * PRIME_PACKET_MS` = 40 ms ahead of the player clock
and topped it up every 20 ms; that queue depth is what NDL's audio renderer paces the video plane
against. The offload route makes real packets the only feed (`yields_to_real`), and fed straight off the
wire they stamp at ≈ the player clock — a packet arrives *after* the frame it was captured with, and
the PTS trim above pulled the shared offset another ~36 ms earlier. Depth ≈ 0, renderer at the edge
of underrun, picture stutters on network jitter: the exact symptom the clock plane was introduced to
cure, back again the moment real audio displaced the metronome. (Found on the since-deleted PCM
route, but the mechanism is the plane's, so it applies to offload identically.)

Fixed by `PLANE_LEAD_MS` (= the same 40 ms), added to every real stamp in `play_audio`. The clock
plane targets the same figure, so silence and real audio meet at the same lead and neither pushes
the other's ceiling. NDL takes no depth argument, so a stamp in the future is the only way to ask it
for one. This is the same job the deleted SDL `JitterPolicy` did with its
25 ms ring (adaptive to 90 under underruns); the route removed that ring and put nothing in its
place. Cost is lip sync, `PLANE_LEAD_MS` behind the picture, roughly cancelling the trim's ~36 ms —
walk it down on device against `lead` on the overlay's audio line and `plane_lead=` on the video
heartbeat, which are the only places the depth is observable. Note the SDL path's own `AvSync` was
measure-only, so there is no prior art for correcting the lip sync, only for holding the depth.

- Unknowns, in order: the depth NDL holds on that plane (it is not `render_buffer_length` and there
  is no query), and whether offload beats software on a set where offload works at all. Both routes
  are named on the overlay (`Opus SW` / `Opus HW`) and in the `audio path:` log line precisely so a
  report says which one produced the numbers.

**3. Cadence pacing: stamps from the cadence loop (the DEFAULT since 2026-08-23, and since
2026-08-27 the ONLY mapping — the `Settings::direct_playback` escape hatch is gone. On a CX at
1440p120 the anchor stamped ~17% of frames late against the loop's ~7%).**

The anchor above was a constant plus a one-off trim, and that shape has two holes. It carries **no
rate term**: two free-running crystals produce a ramp, so the session's real lead walks away over
minutes (either into latency, or into stamps behind the player clock, where NDL gives up pacing) and
nothing pulls it back — trimming stops 3 s in. And its whole jitter margin is `TRIM_KEEP_NS` = 4 ms,
picked for latency and *below the arrival spread of an ordinary link*, so by construction the
latest-arriving frames of every measurement window are stamped in the past. The host's capture is
damage-driven (PipeWire) on top of that, so its cadence is genuinely uneven before the network adds
anything. That combination is the "stutters here, looks fine on the host's own monitor" report.

`session::timeline::CadencePacer` wraps **`punktfunk_core::phase::CadenceClock`** (core v0.30+, the
same loop the desktop/Android/Apple presenters pace on, so all clients compute the same statistic):
a type-2 loop over `ready − pts` whose cushion is `2 × measured MAD`, floored at 0.5 ms and
**capped at one frame interval** — that ceiling is core's invariant, not a knob: past a whole frame
the honest fix is a buffer the user asked for, not a loop quietly holding frames. `snapping()`
tuning, because NDL presents on the panel's grid and the snap-up already carries ~half a refresh.

⚠ **It smooths the offset, never the timestamps.** Core tests that (`preserves_source_cadence`), and
it is what makes the feature honest: a game genuinely rendering at 45 fps still looks exactly as
irregular as it is. Only the transport's contribution is removed.

What it does NOT fix, and no client-side work can: a stream rate that is not the panel rate or an
exact divisor of it. 60 on 120 is fine; 50 on 60 is arithmetic.

Why the anchor stays in the tree at all: it is the mapping every session on this client ran until
now, so it is the known-behaviour comparison for any regression report, and it is the honest answer
for anyone who would rather have the judder than the cushion. `session::timeline::Pacing` is the one
object that owns the choice — nothing above it branches on which mapping is live.

Wiring notes worth knowing before editing:

- **Only the live mapping folds.** Both are stateful over the whole run, so shadowing the idle one
  costs a mapping nobody reads plus a second set of numbers describing a session nobody is watching.
  What makes the two comparable is `late_stamps` — frames whose ACTUAL stamp was already behind the
  player clock, i.e. the judder, counted — which `Pacing` computes from the stamp in use and so
  publishes on both paths. Reported as `pacing:` on the video heartbeat and `Pace` on the overlay.
  The anchor measures no jitter, so its overlay line prints its name where the figure would go
  rather than a zero.
- **One picture folds ONCE.** Slice-progressive delivery repeats an AU's host PTS across its
  pieces at increasing arrival times, so mapping per piece teaches the loop the AU's *tail* arrival
  and inflates the measured jitter by the AU's own transmission time. `VideoStage::au_base_ns` holds
  the stamp while the AU is open — which is also what makes every piece of one AU carry the same
  timestamp, as NDL (start-code boundaries, no AU flag) needs.
- ⚠ **The cushion's ceiling is the STREAM mode's interval, not `frame_interval_ns`.** Those are two
  different quantities that happen to agree on most panels: the reconciled one exists to convert a
  render-queue depth into time, so it follows the panel's drain cadence, while the cushion bounds how
  long a frame may be HELD, so it must follow the cadence the host produces. Core states this with a
  test of its own (`the_cadence_interval_comes_from_the_stream_mode_not_the_panel`) — a 120 fps
  stream on a 60 Hz panel would otherwise license twice the hold the source can justify. The anchor's
  trim ramp takes the same quantity, for the same reason: it pays debt off per frame, and frames come
  from the source.
- **Folded at arrival, which is where core wants it** ("called at SUBMIT rather than at take, so the
  estimate sees the arrival process the transport actually produced"). `snapping()` tuning
  permanently: `SourcePacer::follow` re-tunes to `free_running()` only where VRR is MEASURED live,
  which needs on-glass stamps this platform does not have. `note_off_cadence` is wired by no client,
  including this one — nothing on the wire marks a frame off-cadence.
- **Re-anchor triggers**: the freeze-until-reanchor hold, via `reset_timeline`. Upstream also
  re-anchors on a display change and on a mid-stream mode switch; this client never calls
  `request_mode`, and a host-driven switch arrives with the loss that opens a hold anyway — but the
  interval above is snapshotted at pipeline build, so if mid-session mode changes ever become a real
  path here, that snapshot is the thing to fix.
- **The stamp sequence is clamped monotonic per run** (`last_base_ns`), because the cushion can
  shrink between frames and NDL reads a rewind as a permanent session mute. Cleared on reset, like
  the anchor's own — NDL has been flushed by then. `the_pacer_never_walks_a_stamp_backwards_within_a_run`
  is the gate; it is the one invariant here whose violation costs a session its audio outright.
- **The audio latch gate is gone** (2026-08-28): audio no longer rides this mapping at all, so the
  loop has no audio-facing constraint left. The lip-sync drift this used to carry — the offload
  route riding one latched constant while the video offset kept moving — went with it: both planes
  now advance on the player clock at the same rate.
- Not tried yet, in rough order of expected value: **phase-locked capture** (core has the whole
  protocol — `NativeClient::report_phase` + `CLIENT_CAP_PHASE_LOCK`; the host aligns its capture tick
  to the client's panel grid, which *reduces* latency instead of buffering against it, but needs a
  real vblank anchor and NDL is submit-only, so the anchor would have to come off the graphics plane
  with an unknown constant offset to the video plane's latch); an **adaptive `PLANE_LEAD_MS`** on the
  offload route (the deleted SDL `JitterPolicy` did 25→90 ms under underruns); and **not freezing on
  a one-frame gap** when LTR/RFI recovery is available, since `HOLD_GIVE_UP` is 2 s of frozen picture
  per loss event and each hold re-anchors the timeline.

## ABR startup probe: 2 Gbps, upstream-hardcoded

**"Automatic" bitrate fires a 2 Gbps burst ~2 s into every session, and on Wi-Fi that can cost the session its video entirely** — not a slow start but a flow that never establishes. Measured on G5: a "successful" probe still reported `send_dropped=20211`, i.e. link hammered far past what it can carry (~245 Mbps airlink ceiling), and probes that get nothing back sit on core's 6 s timeout. Capped at 300 Mbps the same link reports `send_dropped=0-167` and stream starts are reliable.

Don't read a slow *start* as this bug — a host compositor coming up has its own startup time, and video legitimately arrives late on first connect of a session. Signal that matters: packet drops on the probe and video that never arrives at all.

**Fixed upstream in core v0.31.3**: the probe target is now `stream_cap_kbps × 2` (still capped at
2 Gbps) rather than a flat 2 Gbps, and a keyframe is requested at probe end when no frame completed
across the burst. `PUNKTFUNK_ABR_PROBE_KBPS` and its `> 0` filter are unchanged, so this client's own
pin below still wins and still reads the same. The history is kept because the pin is why it reads
that way.

This was `CAPACITY_PROBE_KBPS` in `punktfunk-core`'s `client/pump/data.rs` — a **hardcoded const with no cap knob** — directly at odds with this client capping its own speed test for the same "unbounded firehose starves the app" reason (below).

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

## Video backend: NDL

NDL DirectMedia is the only backend. NDL has no decode context; calls go through `NdlVideo::ffi` mutex (header says not thread-safe). AV1 remains disabled (never produced picture).

An SMP (Starfish Media Pipeline) backend for webOS 3.5-4.x was built and removed (2026-08-26, issue #164): never verified on real 3.5-4.x hardware, and it carried a C++ shim `.so`, an ACB sink and a Settings row for the whole NDL v1 audience. Those TVs get NDL v1 (H.264/SDR).

## NDL generations: v2 (webOS 5+) and v1 (3.5-4.x)

- Same library, two ABIs: v2 (`DirectMediaLoad`, `DirectVideoPlay`, `FlushRenderBuffer`, `GetRenderBufferLength`, `SetHDRInfo`) vs v1 (`DirectVideoOpen/SetCallback/SetArea/PlayWithCallback/Close`). webOS 4 has no v2 symbols.
- **Must `dlopen`, never link** `libNDL_directmedia.so.1`: a `DT_NEEDED` breaks webOS 4 startup under BIND_NOW (fails before `main`). Do not re-add `#[link(name = "NDL_directmedia")]`; `-Wl,-z,lazy` is not an acceptable workaround.
- Backend generation comes from `device::ndl_generation()` (`sdkVersion`): v1 for `<5`, v2 for `>=5` and unknown. Version selects what to try; `dlsym` is final authority (no silent fallback).
- v1 limits: H.264 + SDR/BT.709 only; no input PTS, render-buffer query, flush, or HDR API. **Resolution is not capped here**: decode dimensions are passed through; `1920x1080` in `ndl/v1.rs` is only the display rect from `SetArea`.
- `NDL_DIRECTVIDEO_DATA_INFO_T` must include `source` (`width,height,source`). Omitting it fed stack garbage into `NDL_DirectVideoOpen`; now explicitly `NONE` (0) (fixed 2026-08-12).
- M3/KADP runtime codec patch remains intentionally unused.
