#![deny(clippy::unwrap_used)]

#[cfg(not(target_os = "macos"))]
compile_error!("ultraterm currently supports macOS only.");

mod app;
mod headless;
mod input;
mod parser;
mod pty;
mod renderer;
mod screen;
#[cfg(test)]
mod terminal_tests;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if std::env::args().any(|arg| arg == "--headless-self-test") {
        return headless::run_self_test();
    }

    app::run()
}
