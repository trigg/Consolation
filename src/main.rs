use slog::{crit, o, Drain};

static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--winit : Run consolation as a X11 or Wayland client using winit.",
    #[cfg(feature = "udev")]
    "--tty-udev : Run consolation as a tty udev client (requires root if without logind).",
];

fn main() {
    // A logger facility, here we use the terminal here
    let log = if std::env::var("CONSOLATION_MUTEX_LOG").is_ok() {
        slog::Logger::root(
            std::sync::Mutex::new(slog_term::term_full().fuse()).fuse(),
            o!(),
        )
    } else {
        slog::Logger::root(
            slog_async::Async::default(slog_term::term_full().fuse()).fuse(),
            o!(),
        )
    };
    let _guard = slog_scope::set_global_logger(log.clone());
    slog_stdlog::init().expect("Could not setup log backend");

    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            slog::info!(log, "Starting consolation with winit backend");
            consolation::winit::run_winit(log);
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            slog::info!(log, "Starting consolation on a tty using udev");
            consolation::udev::run_udev(log);
        }
        Some(other) => {
            crit!(log, "Unknown backend: {}", other);
        }
        None => {
            println!("USAGE: consolation --backend");
            println!();
            println!("Possible backends are:");
            for b in POSSIBLE_BACKENDS {
                println!("\t{}", b);
            }
        }
    }
}
