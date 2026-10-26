//! `haul`: the engine and its web UI as a local server, for running
//! without the desktop app (or on a machine without a desktop).

use std::{net::Ipv4Addr, path::PathBuf, process::ExitCode};

use clap::Parser;
use haul_core::{Engine, Options, paths};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(
    version,
    about = "Multi-host download manager with a web UI on 127.0.0.1"
)]
struct Cli {
    /// Port for the web UI on 127.0.0.1.
    #[arg(long, env = "HAUL_PORT", default_value_t = 8765)]
    port: u16,
    /// Folder for finished downloads [default: `download_dir` in config.toml, else Downloads/Haul].
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Folder for config.toml, the queue and the instance lock [default: $HAUL_DATA_DIR, else the
    /// platform's local data folder plus `haul`].
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Open the UI in the default browser.
    #[arg(long)]
    open: bool,
    /// Debug mode for agents: every signal streamed at /api/debug/stream,
    /// for a client with the token in `debug-token` (also `HAUL_DEBUG=1`).
    #[arg(long)]
    debug: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(paths::data_dir);
    paths::adopt_old_data(&data_dir);
    // haul.log in the data folder, for a report; the terminal reads along.
    let debug = cli.debug || haul_core::debug::asked();
    haul_core::logging::init(&data_dir, true, debug);
    if debug {
        println!(
            "Debug mode: token in {}",
            haul_core::debug::token_path(&data_dir).display()
        );
    }
    let options = Options {
        data_dir,
        download_dir: cli.dir,
        config: None,
        recycle_bin: None,
    };
    let engine = match Engine::start(options).await {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("haul: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, cli.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("haul: port {}: {e}", cli.port);
            engine.shutdown().await;
            return ExitCode::FAILURE;
        }
    };
    let url = format!("http://127.0.0.1:{}/", cli.port);
    let snap = engine.snapshot();
    println!(
        "Proxy: {}",
        if snap.proxy {
            "on (every host goes through it unless you approve direct)"
        } else {
            "off (direct connections)"
        }
    );
    println!("Haul on {url}  ->  {}", snap.download_dir);
    println!("Data: {}", snap.data_dir);
    if cli.open {
        let _ = open::that_detached(&url);
    }
    let code = tokio::select! {
        served = haul_web::serve(listener, engine.clone(), None) => match served {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("haul: {e}");
                ExitCode::FAILURE
            }
        },
        () = stop_requested() => ExitCode::SUCCESS,
    };
    engine.shutdown().await;
    code
}

/// Ctrl+C, or the system asking the server to stop: SIGTERM from a service
/// manager or `kill`, or the console closing or Windows shutting down. Each
/// leaves time to stop the transfers and save the queue.
async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
        if let (Ok(mut close), Ok(mut shutdown)) = (ctrl_close(), ctrl_shutdown()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
                _ = shutdown.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
