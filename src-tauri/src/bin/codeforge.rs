fn main() {
    if let Err(error) = tauri::async_runtime::block_on(codeforge_desktop::cli::main_entry()) {
        eprintln!("codeforge: {error}");
        std::process::exit(1);
    }
}
