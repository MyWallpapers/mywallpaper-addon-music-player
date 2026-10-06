fn main() {
    if let Err(error) = backend::run() {
        eprintln!("music player companion: {error}");
        std::process::exit(1);
    }
}
