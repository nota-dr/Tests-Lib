use std::fs;
use std::path::PathBuf;

pub fn dir_has_src_files(path: &PathBuf) -> bool {
    let files = match fs::read_dir(path) {
        Ok(files) => files,
        Err(e) => panic!("[!] Error reading directory: {:?}", e),
    };

    for file in files {
        if let Ok(file) = file {
            if let Some(ext) = file.path().extension() {
                if ext == "c" {
                    return true;
                }
            }
        }
    }

    return false;
}
