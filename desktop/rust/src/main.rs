// Release builds are a GUI app (no console window), like the Electron executable.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(windows)]
    {
        nimbo_desktop::shell::windows::host::run(&args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!(
            "Nimbo desktop host requires Windows x64; this platform runs portable tests only."
        );
        std::process::ExitCode::from(2)
    }
}
