use std::error::Error;

use tracing_subscriber::EnvFilter;

pub fn setup_logging() -> Result<(), Box<dyn Error>> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("meikipop_gui=info,meikipop_native=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_thread_names(true)
        .try_init()
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    Ok(())
}
