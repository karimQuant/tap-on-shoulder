use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("tap {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: tap <hook EVENT|sweep|setup|test|doctor|label|classify|--version>");
            ExitCode::from(2)
        }
    }
}
