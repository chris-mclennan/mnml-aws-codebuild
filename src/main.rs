mod app;
mod blit;
#[allow(dead_code)]
mod codebuild;
mod config;
mod keys;
#[allow(dead_code)]
mod log_tail;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-aws-codebuild",
    version,
    about = "AWS CodeBuild + CloudWatch viewer for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,
    /// Blit-host mode — render into a UDS-served cell grid instead
    /// of the local terminal. Used by mnml / tmnl to host this
    /// binary as a pane (`:host.launch mnml-aws-codebuild
    /// --blit /tmp/x.sock`).
    #[arg(long, value_name = "SOCKET")]
    blit: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load()?;

    if cli.check {
        println!("config: {}", config::config_path().display());
        println!("region: {:?}", cfg.region);
        println!("refresh_interval_secs: {}", cfg.refresh_interval_secs);
        for (i, t) in cfg.tabs.iter().enumerate() {
            println!(
                "  tab {} ({}): kind={} project={:?} log_group={:?} log_stream={:?}",
                i + 1,
                t.name,
                t.kind,
                t.project,
                t.log_group,
                t.log_stream
            );
        }
        println!("(auth: defers to the `aws` CLI's own credential chain)");
        return Ok(());
    }

    let mut app = app::App::new(cfg)?;

    if let Some(socket) = cli.blit {
        blit::run(&mut app, std::path::Path::new(&socket)).await
    } else {
        ui::run(&mut app).await
    }
}
