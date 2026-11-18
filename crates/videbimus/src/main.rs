//! The stock `videbimus` binary: the built-in kinds only (ADR 0009 §2).

fn main() -> std::process::ExitCode {
    videbimus::App::new("videbimus").run()
}
