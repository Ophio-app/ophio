# TDV1 encoder measurements

Measured 2026-09-27 on a Linux x86-64 desktop with a 16-core, 32-thread Zen 5 CPU and 64 GB of RAM, Rust 1.98.1, while other work was running; these are local measurements, not isolated lab results. Release profile, scalar tile scheduling, jpeg-encoder 0.6.1 with its SIMD feature, TurboJPEG 3.2.0 through turbojpeg 1.5.1 using the system library. Both use 4:4:4 chroma sampling. The synthetic suite uses no desktop capture or user data. A separate public NASA image measurement is recorded below.

The reproducible `examples/benchmark.rs` workloads draw text-like glyph patterns over light UI panels, a dark code editor, and a photographic-complexity smooth/noisy image. The last is procedural, not a photograph; a separate photographic input is measured below. Each result is the median of nine iterations after a warm-up. End-to-end times include pixel conversion, hashing, tile extraction, JPEG and TDV1 assembly, but exclude native capture and transport. Delta changes a 16×16 region with a matching damage hint. Inputs retain their source size.

Commands:

```sh
cargo run --release -p media --example benchmark
cargo run --release -p media --features turbojpeg --example benchmark
```

| Workload | Source | Q | Rust key ms | Rust delta ms | Turbo key ms | Turbo delta ms | Rust key bytes |
|---|---|---:|---:|---:|---:|---:|---:|
| text_ui | 1920x1080 | 60 | 17.618 | 1.043 | 6.120 | 0.873 | 591573 |
| text_ui | 1920x1080 | 75 | 22.009 | 0.971 | 6.417 | 0.851 | 707990 |
| text_ui | 1920x1080 | 85 | 16.286 | 0.885 | 6.696 | 0.851 | 854953 |
| code_editor | 1920x1080 | 60 | 16.092 | 0.906 | 7.429 | 1.286 | 806709 |
| code_editor | 1920x1080 | 75 | 16.662 | 0.936 | 7.420 | 0.885 | 1050875 |
| code_editor | 1920x1080 | 85 | 18.172 | 0.971 | 8.159 | 0.882 | 1380302 |
| photo_like | 1920x1080 | 60 | 16.881 | 0.955 | 5.606 | 0.856 | 282636 |
| photo_like | 1920x1080 | 75 | 15.432 | 0.921 | 8.139 | 1.359 | 378910 |
| photo_like | 1920x1080 | 85 | 17.770 | 0.995 | 7.963 | 1.357 | 567288 |
| text_ui | 2560x1440 | 60 | 27.236 | 1.530 | 10.995 | 1.506 | 1092424 |
| text_ui | 2560x1440 | 75 | 28.297 | 1.547 | 11.463 | 1.476 | 1310645 |
| text_ui | 2560x1440 | 85 | 30.065 | 1.536 | 12.217 | 1.478 | 1585903 |
| code_editor | 2560x1440 | 60 | 27.845 | 1.542 | 12.322 | 1.525 | 1434498 |
| code_editor | 2560x1440 | 75 | 29.460 | 1.555 | 13.520 | 1.487 | 1868660 |
| code_editor | 2560x1440 | 85 | 32.694 | 1.589 | 14.186 | 1.489 | 2454356 |
| photo_like | 2560x1440 | 60 | 25.532 | 1.538 | 9.882 | 1.490 | 502293 |
| photo_like | 2560x1440 | 75 | 28.981 | 1.610 | 10.383 | 1.515 | 673097 |
| photo_like | 2560x1440 | 85 | 32.104 | 1.594 | 11.664 | 1.504 | 1009464 |

The pure Rust default meets the approximate 40 ms keyframe and 5 ms small-damage targets at 1080p on these workloads. TurboJPEG reduces keyframe CPU time by roughly 2–3×, enough to retain an optional Linux-only `turbojpeg` feature. Default builds require neither a C JPEG library nor cmake/nasm, including Windows. The feature uses pkg-config only and requires a supplied system libturbojpeg.

## Tile size

The same benchmark was repeated with `TILE_EDGE` set to 64 instead of the default 128. At q75:

| Workload | Source | 64 key ms | 128 key ms | 64 delta ms | 128 delta ms | 64 bytes | 128 bytes |
|---|---|---:|---:|---:|---:|---:|---:|
| text_ui | 1920x1080 | 16.102 | 22.009 | 0.821 | 0.971 | 946702 | 707990 |
| code_editor | 1920x1080 | 16.961 | 16.662 | 0.850 | 0.936 | 1289937 | 1050875 |
| photo_like | 1920x1080 | 18.817 | 15.432 | 0.855 | 0.921 | 617336 | 378910 |
| text_ui | 2560x1440 | 29.151 | 28.297 | 1.480 | 1.547 | 1743472 | 1310645 |
| code_editor | 2560x1440 | 31.125 | 29.460 | 1.519 | 1.555 | 2302028 | 1868660 |
| photo_like | 2560x1440 | 28.267 | 28.981 | 1.498 | 1.610 | 1105382 | 673097 |

128-pixel tiles reduce JPEG header overhead substantially without approaching the delta latency budget. They produce 135 JPEG images at 1080p (versus 510 with 64), reducing client decoder and compositor calls. Small deltas may cost more bytes with 128; frame-rate and size adjustment plus a bounded ACK window are still required on constrained links. Nearest-neighbour downscaling preserves sharp pixel edges and permits exact damage-to-tile mapping; photographic resampling quality is a deliberate tradeoff.

## Photographic input

A second run used NASA's [Blue Marble western hemisphere image](https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57723/globe_west_2048.jpg), a 2048×2048 satellite-imagery composite. The benchmark decodes and bilinearly resizes it to each test size before timing; thus these rows measure the same encoding path, without file I/O or input resizing in the timed region. The downloaded asset stays outside version control.

```sh
cargo run --release -p media --example benchmark -- target/benchmark-photo.jpg
cargo run --release -p media --features turbojpeg --example benchmark -- target/benchmark-photo.jpg
```

Source SHA-256: `b79e0577e7565a2c7e9f7165284ccb53ece2872ddc429c687b6d3ae1aaa85780`.

| Source | Q | Rust key ms | Rust delta ms | Turbo key ms | Turbo delta ms |
|---|---:|---:|---:|---:|---:|
| 1920x1080 | 60 | 13.004 | 0.833 | 5.438 | 0.793 |
| 1920x1080 | 75 | 13.377 | 0.831 | 5.628 | 0.789 |
| 1920x1080 | 85 | 13.973 | 0.832 | 5.879 | 0.787 |
| 2560x1440 | 60 | 23.078 | 1.410 | 9.686 | 1.369 |
| 2560x1440 | 75 | 23.622 | 1.414 | 9.959 | 1.358 |
| 2560x1440 | 85 | 24.463 | 1.414 | 10.391 | 1.361 |
