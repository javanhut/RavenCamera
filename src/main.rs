// Pixel and sample loops walk fixed-size groups with `chunks_exact`, which
// reads as what it is; the newer `as_chunks` suggestion is not taken.
#![allow(clippy::chunks_exact_to_as_chunks)]

mod audio;
mod camera;
mod enhance;
mod library;
mod naming;
mod paths;
mod pixels;
mod record;
mod screen;
mod settings;
mod ui;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--probe") {
        probe();
        return glib::ExitCode::SUCCESS;
    }
    if let Some(i) = args.iter().position(|a| a == "--record") {
        return record_from_shell(
            args.get(i + 1).map(String::as_str),
            args.get(i + 2).and_then(|s| s.parse().ok()),
        );
    }
    if let Some(i) = args.iter().position(|a| a == "--finish") {
        // Finish a session folder from a shell, without the window: for
        // recovering a recording by hand, and for testing the encoders.
        let Some(dir) = args.get(i + 1) else {
            eprintln!("usage: raven-camera --finish SESSION_DIR");
            return glib::ExitCode::FAILURE;
        };
        return finish_from_shell(std::path::Path::new(dir));
    }
    ui::run()
}

/// `raven-camera --probe`: what this machine offers, for bug reports.
fn probe() {
    println!("== cameras");
    for dev in camera::devices() {
        println!(
            "{} — {} [{}] {} ({})",
            dev.path.display(),
            dev.name,
            dev.kind_label(),
            dev.bus,
            dev.driver
        );
        for m in &dev.modes {
            println!("    {} {}", camera::v4l2::fourcc_name(m.format), m.label());
        }
        for m in &dev.still_modes {
            println!(
                "    photo {} {}",
                camera::v4l2::fourcc_name(m.format),
                m.label()
            );
        }
        for c in camera::controls::read(&dev.path) {
            println!(
                "    control {}: {} (default {})",
                c.name, c.value, c.default
            );
        }
    }
    println!("== sound");
    let a = audio::devices();
    println!("pw-record available: {}", a.available);
    for d in &a.outputs {
        println!(
            "    output {}{} — {}",
            d.name,
            if d.is_default { " (default)" } else { "" },
            d.description
        );
    }
    for d in &a.inputs {
        println!(
            "    input  {}{} — {}",
            d.name,
            if d.is_default { " (default)" } else { "" },
            d.description
        );
    }
    println!("== screen");
    let (tx, rx) = async_channel::unbounded();
    let _screen = screen::Screen::connect(tx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(e) => println!("    {e:?}"),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    println!("== unfinished recordings");
    for (dir, m) in record::session::leftovers() {
        println!("    {} {:?} {:?}", dir.display(), m.kind, m.state);
    }
}

fn finish_from_shell(dir: &std::path::Path) -> glib::ExitCode {
    let manifest = match record::session::Manifest::load(dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}: {e:#}", dir.display());
            return glib::ExitCode::FAILURE;
        }
    };
    let started = std::time::Instant::now();
    let progress = |p: f64| eprint!("\r{:5.1}%", p * 100.0);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    match record::export::finish(dir, &manifest, &progress, &cancel) {
        Ok(done) => {
            eprintln!(
                "\r{} — {:.1} s of {}×{}, saved in {:.1} s",
                done.path.display(),
                done.length.as_secs_f64(),
                done.size.0,
                done.size.1,
                started.elapsed().as_secs_f64()
            );
            let kind = if manifest.kind == record::session::Kind::Audio {
                library::Kind::Audio
            } else {
                library::Kind::Video
            };
            library::remember(
                &done.path,
                manifest.kind.title(),
                kind,
                Some(done.length),
                done.thumbnail.as_ref(),
            );
            glib::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\n{e:#}");
            glib::ExitCode::FAILURE
        }
    }
}

/// `raven-camera --record camera|audio SECONDS`: record without the window,
/// then save, as the window would. The screen needs the window's Wayland
/// connection to choose a source, so it is not offered here.
fn record_from_shell(what: Option<&str>, seconds: Option<f64>) -> glib::ExitCode {
    let settings = settings::Settings::load();
    let seconds = seconds.unwrap_or(5.0);
    let (kind, camera) = match what {
        Some("camera") => {
            let Some(dev) = camera::devices().into_iter().next() else {
                eprintln!("no camera");
                return glib::ExitCode::FAILURE;
            };
            let Some(mode) = dev.mode_for(&settings.camera.resolution) else {
                eprintln!("the camera has no usable mode");
                return glib::ExitCode::FAILURE;
            };
            camera::controls::restore(&dev.path, settings.camera.controls.get(&dev.key()));
            match camera::Stream::start(&dev, mode) {
                Ok(s) => (record::session::Kind::Camera, Some(std::sync::Arc::new(s))),
                Err(e) => {
                    eprintln!("{e:#}");
                    return glib::ExitCode::FAILURE;
                }
            }
        }
        Some("audio") => (record::session::Kind::Audio, None),
        _ => {
            eprintln!("usage: raven-camera --record camera|audio [SECONDS]");
            return glib::ExitCode::FAILURE;
        }
    };
    let plan = record::live::Plan {
        kind,
        screen: None,
        camera,
        system_audio: settings.recording.system_audio,
        microphone: true,
        subject: String::new(),
    };
    let live = match record::live::Live::start(plan, &settings) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e:#}");
            return glib::ExitCode::FAILURE;
        }
    };
    eprintln!("recording {seconds} s into {}", live.dir.display());
    std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    let frames = live
        .stats
        .camera_frames
        .load(std::sync::atomic::Ordering::Relaxed);
    match live.stop() {
        Ok((dir, m)) => {
            eprintln!("{frames} camera frames, {} audio tracks", m.audio.len());
            let code = finish_from_shell(&dir);
            if code == glib::ExitCode::SUCCESS {
                let _ = std::fs::remove_dir_all(&dir);
            }
            code
        }
        Err(e) => {
            eprintln!("{e:#}");
            glib::ExitCode::FAILURE
        }
    }
}
