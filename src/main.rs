mod autostart;
mod controller;
mod daemon;
mod ipc;
mod protocol;
mod settings;
mod transport;
mod tray;
mod window;

const USAGE: &str = "\
Usage: wavexlr [--hidden | --headless | window | --version]

  (no argument)  start the tray app and open the window
  --hidden       start in the tray without opening the window (autostart)
  --headless     only restore and save the levels, no tray, no window
  window         the window process (started by the tray app)
  --version      print the version";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => daemon::run(daemon::Options { tray: true, show_window: true }),
        ["--hidden"] => daemon::run(daemon::Options { tray: true, show_window: false }),
        ["--headless"] => daemon::run(daemon::Options { tray: false, show_window: false }),
        ["window", ..] => {
            window::run();
            0
        }
        ["--version" | "-V"] => {
            println!("wavexlr {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["--help" | "-h"] => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
