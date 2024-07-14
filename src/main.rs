use std::env;

static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--winit : Run anvil as a X11 or Wayland client using winit.",
    #[cfg(feature = "udev")]
    "--tty-udev : Run anvil as a tty udev client (requires root if without logind).",
];

#[cfg(feature = "profile-with-tracy-mem")]
#[global_allocator]
static GLOBAL: profiling::tracy_client::ProfiledAllocator<std::alloc::System> =
    profiling::tracy_client::ProfiledAllocator::new(std::alloc::System, 10);

fn main() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .compact()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().compact().init();
    }

    #[cfg(feature = "profile-with-tracy")]
    profiling::tracy_client::Client::start();

    profiling::register_thread!("Main Thread");

    #[cfg(feature = "profile-with-puffin")]
    let _server =
        puffin_http::Server::new(&format!("0.0.0.0:{}", puffin_http::DEFAULT_PORT)).unwrap();
    #[cfg(feature = "profile-with-puffin")]
    profiling::puffin::set_scopes_on(true);

    let mut use_winit = false;
    let mut use_tty = false;

    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            tracing::info!("Starting anvil with winit backend");
            use_winit = true;
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            tracing::info!("Starting anvil on a tty using udev");
            use_tty = true;
        }
        Some(other) => {
            tracing::error!("Unknown backend: {}", other);
        }
        None => {
            #[allow(clippy::disallowed_macros)]
            {
                println!("USAGE: anvil --backend");
                println!();
                println!("Possible backends are:");
                for b in POSSIBLE_BACKENDS {
                    println!("\t{}", b);
                }
            }
        }
    }

    if !use_tty && !use_winit {
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
            Err(_e) => tracing::info!("No XDG_SESSION_TYPE environment variable"),
        }
    }

    if use_tty {
        consolation::udev::run_udev();
    } else if use_winit {
        consolation::winit::run_winit();
    }
}
