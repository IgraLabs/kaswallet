use clap::Parser;
use core::args::expand_path;
use ::log::{debug, error, info, trace, warn};

mod args;
mod log;

fn main() {
    let args = args::Args::parse();

    if let Err(e) = log::init_log(expand_path(args.logs_path), args.logs_level.clone().into()) {
        panic!("Failed to initialize logger: {}", e);
    }

    for _ in 0..10_000 {
        trace!("Trace!!!");
        debug!("Debug!!!");
        info!("Info!!!");
        warn!("Warn!!!");
        error!("Error!!!");
    }
}
