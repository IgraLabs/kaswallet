use clap::Parser;

mod args;

fn main() {
    let args = args::Args::parse();

    println!("{:?}", args);
}
