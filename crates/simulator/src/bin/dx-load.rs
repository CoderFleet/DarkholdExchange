use std::error::Error;

use dx_simulator::{LoadConfig, runner};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("dx-load: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", LoadConfig::usage());
        return Ok(());
    }

    let config = LoadConfig::from_args(args).map_err(std::io::Error::other)?;
    let report = runner::run(config.clone()).await?;

    if let Some(path) = &config.json_out {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }

    print!("{}", report.to_markdown());
    Ok(())
}
