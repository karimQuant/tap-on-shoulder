use std::io::Write;
use std::process::ExitCode;

const USAGE: &str =
    "usage: tap <hook EVENT | sweep | setup [--force] [--server URL] | test | doctor | label | classify | --version>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => {
            // A hook must never fail: any panic exits 0 with empty stdout.
            std::panic::set_hook(Box::new(|_| {
                tap::log::Entry::new("panic").kind("panic").write();
                std::process::exit(0);
            }));
            let event = args.get(1).map(String::as_str).unwrap_or("");
            if let Some(out) = tap::hook::run(event) {
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(out.as_bytes());
                let _ = stdout.flush();
            }
            ExitCode::SUCCESS
        }
        Some("sweep") => {
            std::panic::set_hook(Box::new(|_| {
                tap::log::Entry::new("sweep").kind("panic").write();
                std::process::exit(1);
            }));
            tap::sweep::run();
            ExitCode::SUCCESS
        }
        Some("setup") => code(tap::cli::setup::run(&args[1..])),
        Some("test") => code(tap::cli::test_tap()),
        Some("doctor") => code(tap::cli::doctor::run()),
        Some("label") => code(tap::cli::label()),
        Some("classify") => code(tap::cli::classify_stdin()),
        Some("--version") | Some("-V") | Some("version") => {
            print!("{}", tap::cli::version_text());
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | Some("help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn code(c: i32) -> ExitCode {
    ExitCode::from(c.clamp(0, 255) as u8)
}
