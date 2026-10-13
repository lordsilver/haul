//! `haul`: the engine and its web UI as a local server, for running
//! without the desktop app (or on a machine without a desktop).

use std::{net::Ipv4Addr, path::PathBuf, process::ExitCode};

use clap::Parser;
use haul_core::{Engine, Options, paths};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

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
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let options = Options {
        data_dir: cli.data_dir.unwrap_or_else(paths::data_dir),
        download_dir: cli.dir,
        config: None,
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
        _ = tokio::signal::ctrl_c() => ExitCode::SUCCESS,
    };
    engine.shutdown().await;
    code
}
