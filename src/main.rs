fn main() {
    if let Err(err) = mint::cli::run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
