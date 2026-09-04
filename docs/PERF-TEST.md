# Measuring the controller UI on a panel

The shared shell's frame numbers were measured once, on an LG G5. Everything since has been a
guess, and the only report from a 2020 panel is "it lags a bit on my CX" — true, useful, and
not something a change can be checked against.

This branch logs what the shell actually costs on the panel it is running on. Run it, browse a
library, send the lines back.

## Run it

```sh
git fetch origin && git checkout feat/console-shell
task deploy TV_HOST=root@<tv-ip> TELEMETRY=auto TELEMETRY_LEVEL=info
```

`TELEMETRY=auto` streams the log to your terminal, so nothing has to be collected off the TV
afterwards. Leave that terminal open for the whole run.

If you would rather install the `.ipk` by hand, the branch's CI build attaches one, and the same
lines then go to the on-device log — Settings ▸ Diagnostics ▸ Send logs sends it.

## What to do on the TV

The interesting case is a shelf filling with cover art, so:

1. **Connect a game pad** before launching. The controller UI now fronts the app while one is
   attached — if you land in the cursor menus instead, the pad was not seen (a Magic Remote does
   not count, deliberately).
2. Open a host with a **large library** — the more covers the better.
3. Let the shelf load without touching anything, then **scroll from one end to the other**.
4. Switch to the grid, into collections, and back to the shelf.
5. Leave it sitting for a few seconds so a quiet interval gets reported too.

A summary goes out every 10 seconds and once more on the way out.

## What comes back

```
shell library: 412 frames in 10.0s — cpu p50 3.1ms p90 6.8ms p99 22.4ms max 41.0ms
  | art 34 decoded (34 at codec scale) 210ms, mean 6.2ms, worst 19.0ms | up 12s
```

- **cpu p50/p90/p99/max** — CPU-side time per frame, in milliseconds. The vsync wait is not in
  it, so this is what the app spends, not what the panel imposes. A 60 Hz frame is 16.7 ms:
  p99 above that is a visible stutter, p50 above it is the lag you reported.
- **art N decoded (M at codec scale)** — covers decoded in that interval. `M` well short of `N`
  means most art is arriving in a shape the codec's own downscale cannot help with, which is a
  different problem from the panel being slow.
- **total / mean / worst** — what those decodes cost. `worst` is the whole run's worst, not the
  interval's: one bad cover is usually what a stutter is.

The question the numbers answer: **is the shelf's cost the covers?** Compare `art … total`
against the interval. 210 ms of decoding inside 10 s is 2% and not your problem; 4 s of it is.

## What to send

The `shell …` lines, the TV model and webOS version, and roughly how many titles the library
has. If one interval was much worse than the others, say what you were doing during it.

## Notes

- The label after `shell` is where the UI came up: `library` if it opened straight onto a shelf,
  `home` otherwise.
- Decoding is capped at 6 ms per frame, so a slow panel spreads a library load over more frames
  rather than dropping them. A shelf that fills *slowly* but scrolls smoothly is that cap doing
  its job; tell us if it feels too slow to fill and the cap can move.
- Frame times are CPU-side only. If the numbers look fine and it still feels bad, the cost is
  somewhere this does not measure — say so, because that is worth knowing.
