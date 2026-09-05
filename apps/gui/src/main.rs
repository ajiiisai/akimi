mod app;
mod startup;

fn main() {
    if let Err(error) = startup::prepare_display() {
        eprintln!("Akimi cannot start: {error}");
        std::process::exit(1);
    }
    app::run();
}
