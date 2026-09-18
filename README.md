# Raven Camera

Photos, video, screenshots and screen recording for Raven Linux, in one
window: the built-in camera or any USB webcam, a whole screen, one window or
a region you drag out, with system sound, the microphone and a webcam
overlay. GTK 4 + libadwaita in Rust, in Raven Glass like the other Raven
applications, laid out after the design mockup.

```
make            # build (release)
make run        # run it
make probe      # print the cameras, sound devices and screen capture this machine has
make check      # fmt, clippy, tests
sudo make install
```

or with imlazy: `imlazy build`, `imlazy run`, `imlazy probe`.

It builds against the RavenGUI checkout beside this one — `../RavenGUI` —
because its encoders are RavenGUI crates; `.cargo/config.toml` says so.
Delete that file to build against the git versions instead.

## Pages

| Page | What it does |
|---|---|
| Capture | The studio. Screen, Window, Region, Camera or Audio Only; a live preview; record, pause, stop, screenshot or photo from the floating bar; recording settings and where files go beside it; the latest captures under it. |
| Record | The recording in progress with its sound meters, recordings being saved with their progress, and recordings a crash interrupted, which can still be saved. |
| Screenshot | A screen, a window or a region, after a delay, with or without the pointer, copied to the clipboard. |
| Camera | The viewfinder on its own: photo and video, self-timer, grid, mirror, resolution, and the camera's own picture controls (brightness, white balance, exposure — whatever that camera has). |
| Media | Everything captured, filtered by kind. Open, show in folder, copy, rename, trash. |
| Settings | Folders, naming, quality, video size, overlay, and what this machine offers. |

From a shell: `raven-camera --stop` stops a recording (the dock's right-click
menu has it too, for a recording whose window was minimised out of the way),
`--screenshot`, `--record camera|audio SECONDS`, `--finish SESSION_DIR`,
`--probe`.

## How it works

Nothing is faked, and nothing outside Raven decides whether a recording can
be saved.

| Part | How |
|---|---|
| Cameras | V4L2 ioctls straight to `/dev/videoN` (`src/camera/v4l2.rs`, every struct pinned to the kernel ABI by a test). Every UVC camera, built in or USB, goes through the same path; `/dev` is watched, so a camera plugged in shows up at once. MJPEG, YUYV and NV12. |
| The screen | `raven_capture_v1` from Huginn's `raven_shell_v1` version 4 — a screen, one window on its own, or a region, into `wl_shm` buffers this app supplies, with the pointer and click rings drawn by the compositor when asked. Region selection is Huginn's own. Against an older compositor, screen capture says it is unavailable and everything else works. |
| Sound | `pw-record`, PipeWire's own tool, the way Huginn and Oracle use it: the output's monitor for system sound, any input for the microphone. Each is its own track, mixed when the recording is saved. |
| While recording | Nothing is encoded. The screen goes into Raven's lossless screen format (`raven-rec`), the camera as the JPEG frames it sent, sound as WAV, all against one clock that pause stops. A Celeron cannot encode H.264 in real time; it can do this. |
| Saving | Raven's own H.264 (`raven-h264`), AAC (`raven-aac`) and MP4 (`raven-mp4`). The video is cut at key frames and each piece encoded on its own core; sound is woven in so the file plays straight through. Recordings live in `~/.local/share/raven-camera/sessions` until saved, so a crash loses nothing that was captured. |
| Photos and screenshots | PNG or JPEG, pure Rust. |

## Known limits

- MIPI cameras on recent Intel laptops (IPU6) need libcamera, which Raven does
  not ship; those cameras do not appear. USB and built-in UVC cameras do.
- Saving takes time on a slow machine: roughly half the recording's length for
  720p camera video on four Celeron cores, much less for a mostly still screen.
  It happens in the background, and the Record page shows its progress.
