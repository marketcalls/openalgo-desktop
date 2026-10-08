// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `openalgo-desktop mcp ...`: the MCP stdio server for AI clients. It
    // talks to the running app over loopback and never starts Tauri or
    // opens the data directory (crate::mcp::stdio).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mcp") {
        std::process::exit(openalgo_desktop_lib::mcp::stdio::main(&args[2..]));
    }
    openalgo_desktop_lib::run()
}
