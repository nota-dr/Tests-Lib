use std::io::Write;
use std::process::ExitStatus;
use tokio::io::AsyncReadExt;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

#[allow(dead_code)]
#[derive(Debug)]
pub enum Status {
    Timeout = 124 << 8,
    Sigint = 130 << 8,
    Sigabrt = 134 << 8,
    Sigkill = 137 << 8,
    Sigsegv = 139 << 8,
    Sigpipe = 141 << 8,
}

pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: Result<ExitStatus, std::io::Error>,
}

impl ProcessOutput {
    pub fn new(
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        status: Result<ExitStatus, std::io::Error>,
    ) -> Self {
        Self { stdout, stderr, status }
    }
}

pub async fn pipe_reader<R>(mut pipe: R) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut temp_buf = [0u8; 1024];
    while let Ok(n) = pipe.read(&mut temp_buf).await {
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&temp_buf[..n]);
    }
    buffer
}

pub struct TestSpawner {
    child: tokio::process::Child,
    out_task: Option<tokio::task::JoinHandle<Vec<u8>>>,
    err_task: Option<tokio::task::JoinHandle<Vec<u8>>>,
}

impl TestSpawner {
    pub async fn new(
        cmd_args: &Vec<String>,
        cwd: &std::path::PathBuf,
        startup_delay: u64,
    ) -> Self {
        // construct the path to the executable
        let elf_path = cwd.join(&cmd_args[0]);

        // check if the executable exists
        if !elf_path.exists() && elf_path.file_name().unwrap() != "valgrind" {
            panic!("[-] Cannot run exercise, {:?} is not found", elf_path);
        }

        let mut child = tokio::process::Command::new(&cmd_args[0])
            .args(&cmd_args[1..])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("[!] Failed to start child process");

        if startup_delay > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(startup_delay))
                .await;
        }

        let stdout = child.stdout.take().expect("[!] Failed to get stdout");
        let stderr = child.stderr.take().expect("[!] Failed to get stderr");

        // Spawn asynchronous tasks to handle stdout and stderr
        let out_task = tokio::spawn(pipe_reader(stdout));
        let err_task = tokio::spawn(pipe_reader(stderr));

        Self {
            child,
            out_task: Some(out_task),
            err_task: Some(err_task),
        }
    }
}

impl TestSpawner {
    pub fn id(&self) -> Option<i32> {
        match self.child.id() {
            Some(pid) => Some(pid as i32),
            None => None,
        }
    }
}

impl TestSpawner {
    pub async fn wait(&mut self, finish_timeout: u64) -> ProcessOutput {
        let secs = tokio::time::Duration::from_secs(finish_timeout);
        let result = tokio::time::timeout(secs, async {
            let status = self.child.wait().await;
            status
        })
        .await;

        let result = match result {
            Ok(status) => status,
            Err(_timeout) => {
                self.child.kill().await.unwrap();
                self.child.wait().await.unwrap();
                // timed out - return corresponding status code
                Ok(ExitStatus::from_raw(Status::Timeout as i32))
            }
        };

        let stdout = self
            .out_task
            .take()
            .unwrap()
            .await
            .expect("[-] Failed to read stdout");

        let stderr = self
            .err_task
            .take()
            .unwrap()
            .await
            .expect("[-] Failed to read stderr");

        ProcessOutput::new(stdout, stderr, result)
    }
}

pub fn compile(input: &str, cwd: &std::path::PathBuf) -> String {
    let args: Vec<&str> = input.split_whitespace().collect();
    if args.len() < 5 {
        panic!("[!] Invalid gcc input: {}", input);
    }

    let elf_path = cwd.join(args.last().as_ref().unwrap());
    if elf_path.exists() {
        std::fs::remove_file(elf_path)
            .expect("[-] Failed to remove existing executable");
    }

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(input)
        .current_dir(cwd)
        .output()
        .expect("[-] Failed to run compilation command");

    let log_path = cwd.join("compilation_output.txt");
    let mut logfile = std::fs::File::create(&log_path).expect(
        format!("[-] Failed to create compilation log file: {:?}", &log_path)
            .as_str(),
    );

    logfile
        .write_all(&output.stdout)
        .expect("[-] Failed to write to compilation log file");

    logfile
        .write_all(&output.stderr)
        .expect("[-] Failed to write to compilation log file");

    let needle = b"error:";
    if let Some(_) = output
        .stderr
        .windows(needle.len())
        .position(|window| window == needle)
    {
        return String::from("error");
    }

    let needle = b"warning";
    if let Some(_) = output
        .stderr
        .windows(needle.len())
        .position(|window| window == needle)
    {
        return String::from("warning");
    }

    String::from("success")
}

pub fn check_valgrind_leaks(log_path: &std::path::PathBuf) -> bool {
    let log_contents = match std::fs::read_to_string(log_path) {
        Ok(contents) => contents,
        Err(_) => {
            println!("[-] Failed to read valgrind log file");
            return false;
        }
    };

    let needle = "ERROR SUMMARY: 0 errors from 0 contexts";
    if log_contents.contains(needle) {
        true
    } else {
        false
    }
}
