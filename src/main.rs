use slog::{o, Drain};
use std::env;

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
    let mut use_winit = false;
    let mut use_tty = false;

    match env::var("XDG_SESSION_TYPE") {
        Ok(val) => {
            if val == "tty" {
                use_tty = true;
            } else if val == "wayland" {
                use_winit = true;
            } else if val == "x11" {
                use_winit = true;
            }
        }
        Err(_e) => slog::info!(log, "No XDG_SESSION_TYPE environment variable"),
    }

    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            slog::info!(log, "Opting for Winit");
            use_tty = false;
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            slog::info!(log, "Opting for TTY/udev");
            use_winit = false;
        }
        _ => {
            println!("USAGE: consolation --backend");
            println!();
            println!("Possible backends are:");
            for b in POSSIBLE_BACKENDS {
                println!("\t{}", b);
            }
        }
    }
    if use_winit {
        consolation::winit::run_winit(log);
        return;
    }
    if use_tty {
        consolation::udev::run_udev(log);
        return;
    }
}
