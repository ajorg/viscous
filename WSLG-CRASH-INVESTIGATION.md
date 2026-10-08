# viscous dies under WSLg: investigation report

Working notes, complete enough to distill into two bug reports. Written 2026-08-25.

---

## 1. Summary

Running `viscous` (or any winit application) under WSLg kills the compositor.
The client sees its Wayland socket hang up; weston takes `SIGSEGV` and WSLg
restarts it. Nothing in viscous violates the Wayland protocol.

The trigger is `wl_shm_pool.resize`. Smithay's client toolkit creates the
client-side-decoration buffer pool **one byte long** and grows it by doubling,
so drawing a title bar issues five or six `wl_shm_pool.resize` requests. Each
one asks the compositor to re-map a pool it still has buffers attached from.
weston 9.0.0 with `rdprail-shell` survives some and segfaults on others.

Two separate defects:

| | Where | What |
|---|---|---|
| **A** | Smithay client-toolkit / sctk-adwaita | `SlotPool::new(1, shm)` — a decoration pool that starts at one byte and doubles, causing 5–6 pool re-maps to draw one title bar. Wasteful everywhere; fatal here. |
| **B** | WSLg (weston 9.0.0 + `rdprail-shell`) | Segfaults on a legal `wl_shm_pool.resize` of a mapped surface's pool. Only at output scale 2; never observed at scale 1. |

Fixing **A** (pre-size the pool) eliminates the crash completely. **B** is still
a compositor bug and should be reported regardless.

---

## 2. Environment

```
WSLg              1.0.73.2, built 2026-05-18, aarch64 (Snapdragon)
Azure Linux       3.0.20260510            (WSLg system distro)
weston            9.0.0  build 9.0.0-213-g04d436c7
                  commit 04d436c7d9a0cd55fa64b6612c7fa678d6fcd077
FreeRDP           c4030980b29322a9cb2190711a5fadeeeb8b6a33
launched as       weston --backend=rdp-backend.so --modules=wslgd-notify.so
                          --xwayland --socket=wayland-0 --shell=rdprail-shell.so
output rdp-0      2944x1840, desktopScaleFactor 200, deviceScaleFactor 180
                  weston scale 2 -> 1472x920 logical
GPU               none. "Failed to initialize glamor, falling back to sw";
                  mesa runs swrast, so even the main surface goes through wl_shm
```

Client stack:

```
eframe 0.35.0 (glow, accesskit, wayland, x11)   egui 0.35.0
winit 0.30.13
smithay-client-toolkit 0.19.2                   <- winit's frame + shm pools
sctk-adwaita 0.10.1                             <- only when winit's
                                                   wayland-csd-adwaita is on
smithay-clipboard 0.7.3 (pulls sctk 0.20.0, unrelated)
```

### Compositor globals (from `WAYLAND_DEBUG=1`)

```
weston_rdprail_shell   weston_screenshooter   wl_compositor (bound v4)
wl_data_device_manager wl_output (v3)         wl_seat
wl_shell               wl_shm (v1)            wl_subcompositor
wp_presentation        wp_viewporter          xdg_wm_base
zwp_input_method_v1    zwp_input_panel_v1     zwp_input_timestamps_manager_v1
zwp_pointer_constraints_v1                    zwp_relative_pointer_manager_v1
zwp_text_input_manager_v1                     zxdg_output_manager_v1
zxdg_shell_v6
```

Two consequences worth recording:

- **No `zxdg_decoration_manager_v1`.** Server-side decorations cannot be
  negotiated; every client must draw its own. WSLg does not draw host-side
  frames for undecorated toplevels either (verified: an undecorated window
  appears with no title bar).
- **`wl_compositor` at version 4.** `wl_surface.preferred_buffer_scale` needs
  version 6, so a client cannot learn its scale before the surface is mapped.
  The scale-1 draw followed by a scale-2 redraw is forced.

---

## 3. Symptom

Client side:

```
Io error: Broken pipe (os error 32)
Io error: Broken pipe (os error 32)
Io error: Broken pipe (os error 32)
winit EventLoopError: Exit Failure: 1
```

`strace` shows `ppoll(...) = 1 ([{fd=3, revents=POLLIN|POLLHUP}])` — the
compositor hangs up — then the `EPIPE` writes.

Compositor side, `/mnt/wslg/stderr.log`:

```
WSLGd: Run:110: pid NNNN terminated with signal 11, /usr/bin/weston --backend=rdp-backend.so ...
```

**No `wl_display.error` in any trace, ever.** The compositor never reports a
protocol violation; it dies.

---

## 4. Root cause

### 4.1 The allocator

`smithay-client-toolkit-0.19.2/src/shell/xdg/fallback_frame.rs:103`
and `sctk-adwaita-0.10.1/src/lib.rs:108` both contain:

```rust
let pool = SlotPool::new(1, shm)?;
```

`SlotPool::new` seeds its free list with the whole pool, so a one-byte pool has
one byte free. Growth is in `smithay-client-toolkit-0.19.2/src/shm/slot.rs:214`:

```rust
// resize like Vec::reserve, always at least doubling
let target = std::cmp::max(rv + size, self.inner.len() * 2);
self.inner.resize(target)?;
```

which bottoms out in `src/shm/raw.rs:67`:

```rust
self.mem_file.set_len(size as u64)?;   // ftruncate
self.pool.resize(size as i32);         // wl_shm_pool.resize — compositor re-maps
self.mmap = unsafe { MmapMut::map_mut(&self.mem_file) }?;
```

The backing fd is a `memfd` (tmpfs), not a file on a WSL mount — that was
checked and is not a factor.

### 4.2 Why the count can't be reduced

The frame draws five parts (header, four borders) and allocates them **one at a
time**, each followed by attach and commit. From a one-byte pool:

- part 1 finds nothing free → resizes to *exactly* its own size, leaving the
  pool full and the free list empty;
- part 2 therefore forces a doubling;
- parts 3–5 fit in the slack.

**Two resizes minimum, for a window of any size.** Simulated against the real
allocator (model validated — it reproduces the observed sequence exactly):

```
today: 800x600, then fit, then scale 2    4 resizes   peak 614,408
created at fitted size, then scale 2      4 resizes   peak 527,620
created fitted, scale known up front      2 resizes   peak 351,746
a 1x1 window                              4 resizes   peak   1,282
```

A one-pixel window still takes four. Window geometry, ordering and sizing
discipline do not touch the count — it is a property of the allocator's shape.

### 4.3 Why the resize is dangerous

The requests interleave (from `trace-default.log`):

```
resize(156288)                                    <- nothing attached yet   SAFE
create_buffer(#45); attach(#45); commit           <- #45 now live
resize(312576)                                    <- re-map with #45 attached
create_buffer(#47); attach(#47); commit
resize(625152)                                    <- re-map with #45,#47 attached
```

Only the *first* resize is safe. Every later one asks the compositor to
`mremap` a pool it is actively compositing buffers out of. weston's pixman
renderer caches the raw pixel pointer when a buffer is attached; if the re-map
relocates the mapping, the next composite dereferences freed memory.

That also explains the intermittency — it is a race against weston's repaint
timer, not a deterministic protocol fault.

### 4.4 Mapped vs unmapped — the sharpest evidence

Splitting resizes by whether the toplevel had mapped yet (three consecutive
runs of the same binary, `trace-50.log`):

```
run1: first main-surface commit at line 290, pipe at 366
    resize(   41,920) at 233   before map
    resize(   83,840) at 245   before map
    resize(  167,680) at 268   before map
    resize(  335,360) at 309   AFTER map
    resize(  670,720) at 321   AFTER map
    resize(1,341,440) at 355   AFTER map     <- death follows this
```

**Pre-map resizes are always survived. Deaths always follow a post-map one.**
Consistent with the pixman-pointer theory: before the map there is nothing
being composited, so there is no cached pointer to dangle.

---

## 5. Evidence

### 5.1 Every death follows a resize

24 runs across six window sizes, `egui_hello` (stock eframe hello-world, no
viscous code at all):

```
file    run size       died   lastResize  pipe   gap(lines)
200     1   300x200    True   471         490    19
200     2   150x50     True   355         366    11
200     3   150x50     False  370         -      -
300     1   400x300    True   471         490    19
480     1   640x480    True   480         503    23
50      1-4 150x50     True   355         366    11   (x4, identical)
50      5   150x50     False  355         -      -
50      6   150x50     False  370         -      -
50      7   150x50     True   355         366    11
50      8   150x50     False  370         -      -
700     1   1100x700   True   502         521    19
700     2,3 1100x700   True   344         366    22
700     4,5,6 1100x700 False  539/590/727 -      -
700     7   1100x700   True   780         799    19
700     8,9 1100x700   True   344         366    22
700     10  1100x700   True   681         700    19
default 1   800x600    True   979         998    19
```

**15 deaths, every one 11–23 lines after a `wl_shm_pool.resize`. No surviving
run has a resize in its tail.**

Pool size is *not* the variable: 150x50 dies with a 1.3 MB pool while 1100x700
survives with 13 MB. Same size both crashes and survives — it is a race.

### 5.2 Output scale is decisive

Same unpatched binary, Windows display scaling changed to 100% and
`wsl --shutdown` to restart weston:

```
scale 2:  never survived   (2 post-map resizes: 307,200 survived, 614,400 fatal)
scale 1:  11/11 survived   (1 post-map resize:  307,200), 3 focus changes each
```

At scale 1 there is no `set_buffer_scale(2)` anywhere in the trace and the
scale-1 -> scale-2 decoration redraw generation does not exist, removing exactly
the resize that kills.

Caveat, recorded honestly: an earlier single scale-1 run *did* contain a
614,400 resize and survived it. So this is not a clean size threshold. Three
explanations remain unseparated by the data — the number of post-map resizes,
the size of the mapping being re-mapped, or the fact that at scale 2 weston
composites those subsurfaces through a scaling path rather than a 1:1 blit.

### 5.3 The pool stabilises

In a 6,867-line scale-1 session with three focus-loss/regain cycles, only the
**first** caused a resize. After the pool reached its working size it never grew
again. The danger window is bounded to the first seconds of a process's life; a
process that survives startup is safe indefinitely.

### 5.4 What the working programs do differently

- **GTK3** draws its decorations inside its own toplevel surface — 0 subsurfaces,
  no separate decoration pool.
- **es2gears / libdecor** uses 2 subsurfaces and per-part buffers; a focus
  repaint resizes no pool. `libdecor 0.2.2` is installed and `es2gears_wayland`
  links it directly.
- **mesa**, in the *same process* as the crash, uses the safe pattern —
  `create_pool` at exactly the needed size, `create_buffer`, `pool.destroy()`,
  never a resize — 14 times across a 19,000-line trace without incident.

Only winit's frame resizes a pool that has buffers attached.

---

## 6. The fix, and its verification

One line, in whichever frame winit is compiled with:

```rust
// smithay-client-toolkit-0.19.2/src/shell/xdg/fallback_frame.rs:103
// sctk-adwaita-0.10.1/src/lib.rs:108
let pool = SlotPool::new(64 * 1024 * 1024, shm)?;   // was SlotPool::new(1, shm)
```

64 MiB is sparse — pages are backed only when touched — and `SlotPool::new`
puts the whole length in the free list, so every allocation is satisfied from
the free list and **no `wl_shm_pool.resize` is ever issued**.

Verified: `wl_shm#9.create_pool(..., 67108864)` and zero resize lines in the
trace; `egui_hello` and `viscous` both survive startup and repeated focus
changes at scale 2, where they previously failed every time.

**Which crate to patch depends on a cargo feature.** winit selects the frame at
compile time (`winit-0.30.13/src/platform_impl/linux/wayland/window/state.rs:47-50`):
`AdwaitaFrame` when the `sctk-adwaita` feature is on, `FallbackFrame` otherwise.
eframe does not enable winit's default features, so **viscous ships with
`FallbackFrame`**. Distinguishable in a trace by geometry: Fallback has a 24 px
header and 4 px borders; Adwaita has 35 px and 44 px.

---

## 7. Alternatives ruled out

| Avenue | Why not |
|---|---|
| Config / env var / cargo feature for the pool size | None exists; the literal is in both frames |
| Server-side decorations | No `zxdg_decoration_manager_v1` advertised — the only mechanism |
| WSLg host-drawn frames | Undecorated toplevels get no title bar (verified) |
| winit + libdecor | Upstream: libdecor-rs "nowhere near completion, no one working on it" — sctk-adwaita exists *because* of that |
| SDL2 / GLFW backend (both use libdecor) | `egui_glfw_gl` dead since 2021; `egui_sdl2_gl` tracks egui 0.33 vs our 0.35, loses eframe/AccessKit/clipboard/persistence, adds `SDL2.dll` to a single-file Windows deliverable |
| Different renderer (wgpu, software) | Mesa is not implicated — see 5.4 |
| Size the window at creation | Same resize count; at scale 2 it is *worse* (3 post-map vs 2) |
| Oversized pre-map birth (grow the pool before mapping) | Reaches 0 post-map resizes at a 3850 px birth width — but only for a window the user never resizes. First drag re-introduces the crash. A booby trap, not a fix |
| Run the desktop at 100% | Works (11/11) but cannot be required of a user's machine |
| Draw our own decorations | Genuinely works; costs permanent ownership of decorations, drag, and eight resize edges. Fallback if the patch is unacceptable |

---

## 8. Dead ends and corrections

Recorded so they are not re-run.

- **Not the renderer.** Rebuilt with `wgpu` instead of `glow` — fails identically.
- **Not the release profile.** Debug build fails identically.
- **Not a protocol violation.** Full audit against `xdg-shell.xml`: initial
  commit with no buffer, wait for `xdg_surface.configure`, `ack_configure`, then
  attach — winit does all of this correctly and blocks inside `create_window`
  until the initial configure arrives. The first configure is `(0, 0, array[0])`,
  the spec's "client should decide its own window dimension".
- **`with_visible(false)` / `ViewportCommand::Visible(true)` are no-ops on
  Wayland.** `winit .../wayland/window/mod.rs:253` — body is the comment
  "Not possible on Wayland." xdg-shell has no visibility toggle at all.
- **`with_icon` is a no-op on Wayland.** `.../wayland/window/mod.rs:433` is an
  empty function, and winit 0.30 has no `xdg-toplevel-icon-v1` support. The
  supported route is `set_app_id` matching a `.desktop` basename; eframe
  defaults app_id to the app name (`"Viscous"`), which is not one. **Separate
  real bug, unrelated to the crash.**
- **Crash counts from `/mnt/wslg/stderr.log` are unreliable.** The log is reset
  when WSLg restarts; a count read as 30 later read as 8. Any comparison built
  on those counts is void.
- **`egui_hello` and `viscous` were using different frames.** Adding
  `winit = "0.30.13"` to `egui_gui`'s `[dev-dependencies]` (for a throwaway
  probe) pulled winit's default features into everything built *with*
  dev-dependencies — examples and tests — but not the binary. Examples got
  `AdwaitaFrame`, the binary got `FallbackFrame`. Any conclusion drawn from
  comparing the two before this was found needs re-checking.
- **Focus loss is not the trigger**, though it looked like it for a while. Nine
  of the fifteen deaths never received a keyboard `leave` at all and died
  mid-startup. Focus loss is merely one occasion that forces a decoration
  redraw, hence a resize.

---

## 9. Reproduction

Minimal client: stock eframe hello-world, no application code.

```rust
fn main() {
    let _ = eframe::run_native(
        "egui hello",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(Hello) as Box<dyn eframe::App>)),
    );
}
```

1. WSLg with output scale 2 (Windows display scaling 200%).
2. Run it. Click away to another window, then back, a few times.
3. It dies with three `Broken pipe` lines within seconds to a minute;
   `/mnt/wslg/stderr.log` gains a `signal 11` line.
4. `WAYLAND_DEBUG=1` shows the last decoration-pool `resize` 11–23 lines before
   the hangup.
5. At Windows scaling 100% (after `wsl --shutdown`) it does not reproduce.

---

## 10. Two bug reports to write

**To Smithay/client-toolkit** — `SlotPool::new(1, shm)` in `fallback_frame.rs`
(and the same in sctk-adwaita). Starting a decoration pool at one byte forces
5–6 `wl_shm_pool.resize` round-trips to draw one title bar, on every compositor,
for no benefit — the frame knows its own size. Include the allocator arithmetic
from §4.2 and note that upstream master still has the literal. One-line fix.

**To microsoft/wslg** — weston 9.0.0 + `rdprail-shell` segfaults on a legal
`wl_shm_pool.resize` when the pool has buffers attached to a mapped, scaled
surface. Reproduces reliably at output scale 2, not at all at scale 1 (11 clean
runs). No `wl_display.error` is ever sent — the compositor dies rather than
rejecting anything. Include §5.1, §5.2 and §9. Likely a cached pixel pointer in
the pixman renderer surviving an `mremap` that relocated the mapping.

---

## 11. Open questions

- Which of the three candidates explains the scale-2 dependence: post-map resize
  count, size of the re-mapped region, or weston's scaled-composition path.
  Separating them needs a client that can hold two of the three constant.
- Whether any actively maintained compositor other than weston 9 is affected.
  Untested — one compositor is the entire sample. This is the main argument for
  fixing the pool rather than special-casing WSLg.
- Whether `mremap` actually relocates in the failing cases. Confirmable only
  from weston's side (a debug build, or a core dump from the system distro).

---

## 12. State of the working tree

Temporary, to be reverted:

- `Cargo.toml` — `[patch.crates-io]` pointing `sctk-adwaita` and
  `smithay-client-toolkit` at vendored copies in the session scratchpad.
  Must become a git fork (or disappear) before this is anything but a test rig.
- `egui_gui/Cargo.toml` — `winit = "0.30.13"` in `[dev-dependencies]`; the
  feature-unification hazard in §8. Remove.
- `egui_gui/src/main.rs` — `PROBE_SHOWN`, `PROBE_NOICON`, `PROBE_UNDECORATED`,
  `PROBE_NOZOOM`, `PROBE_NOFIT` switches. Remove.
- `egui_gui/examples/egui_hello.rs`, `egui_gui/examples/winit_only.rs` — throwaway.
- `ltrace.log`, `strace.log` at the project root.

Real bugs found along the way, worth fixing on their own merits:

- `main()` prints `NO_WINDOW_HINT` ("viscous needs a display…") on *any* error
  from `run_native`, including a compositor crash on a perfectly good display.
  Misleading; needs to distinguish "no display" from "the display went away".
- No `app_id` matching a `.desktop` file, so the window icon cannot work on
  Wayland (§8).
- viscous draws a full generation of decorations for the 800×600 default window
  it never shows, before `fit_window_to` shrinks it. Wasteful (largest of the
  three generations at scale 1). Note it does *not* reduce crash exposure —
  fixing it is correctness, not mitigation.
