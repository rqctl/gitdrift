use clap::Parser;

fn main() {
    let args = gitdrift::cli::Args::parse();
    match gitdrift::cli::run(args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("gitdrift: {e:#}");
            std::process::exit(2);
        }
    }
}
