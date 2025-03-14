use super::run::*;
use crate::ProcessOutput;
use async_trait::async_trait;
use indexmap::IndexMap;
use libc;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

pub struct CommunicateOutput {
    pub output: Vec<Vec<u8>>,
    pub error: Option<std::io::Error>,
}

#[allow(unused_variables)]
#[async_trait]
pub trait TestAgent: Send + Sync {
    async fn validate(
        &self,
        args: &Vec<String>,
        communicate_output: Option<CommunicateOutput>,
        output: ProcessOutput,
        cwd: &std::path::PathBuf,
    ) -> bool {
        unimplemented!("Must be implemented by the type")
    }

    async fn communicate(
        &self,
        read_timeout: u64,
        port: &str,
        process_id: Option<i32>,
    ) -> CommunicateOutput {
        unimplemented!("Must be implemented by the type")
    }
}

pub struct TestTemplateBuilder {
    // pass through new method
    name: String,
    cmd_args_template: String,
    test_factory: Option<Box<dyn Fn() -> Box<dyn TestAgent>>>,
    timeout: u64,
    // validator builder attributes
    valgrind: bool,
    log_output: bool,
    // communicator builder attributes
    communicate: bool,
    operation_timeout: u64,
}

pub struct TestTemplate {
    name: String,
    cmd_args_template: String,
    test_factory: Box<dyn Fn() -> Box<dyn TestAgent>>,
    timeout: u64,
    valgrind: bool,
    log_output: bool,
    require_communicator: bool,
    operation_timeout: u64,
}

pub struct Test {
    name: String,
    cmd_args: Vec<String>,
    test: Box<dyn TestAgent>,
    timeout: u64,
    log_output: bool,
    require_communicator: bool,
    operation_timeout: u64,
    port: u16,
}

pub struct TestManager<'a> {
    pub name: &'a str,
    pub tests_dir_path: PathBuf,
    startup_delay: u64,
    templates: IndexMap<String, TestTemplate>,
    active_tests: IndexMap<String, Test>,
}

impl TestTemplateBuilder {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            cmd_args_template: String::new(),
            test_factory: None,
            timeout: 0,
            // validator builder attributes
            valgrind: false,
            log_output: false,
            // communicator builder attributes
            communicate: false,
            operation_timeout: 0,
        }
    }

    pub fn args_template(mut self, cmd_args_template: &str) -> Self {
        self.cmd_args_template = cmd_args_template.to_string();
        self
    }

    pub fn agent(mut self, test: Box<dyn Fn() -> Box<dyn TestAgent>>) -> Self {
        self.test_factory = Some(test);
        self
    }

    pub fn timeout(mut self, validator_timeout: u64) -> Self {
        self.timeout = validator_timeout;
        self
    }

    pub fn valgrind(mut self, valgrind: bool) -> Self {
        self.valgrind = valgrind;
        self
    }

    pub fn log_output(mut self, log_output: bool) -> Self {
        self.log_output = log_output;
        self
    }

    pub fn communicate(mut self, require_communicator: bool) -> Self {
        self.communicate = require_communicator;
        self
    }

    pub fn operation_timeout(mut self, communicator_timeout: u64) -> Self {
        self.operation_timeout = communicator_timeout;
        self
    }

    pub fn build(self) -> TestTemplate {
        if self.test_factory.is_none() {
            panic!("[-] Test factory is required");
        }

        TestTemplate {
            name: self.name,
            cmd_args_template: self.cmd_args_template,
            test_factory: self.test_factory.unwrap(),
            log_output: self.log_output,
            valgrind: self.valgrind,
            require_communicator: self.communicate,
            timeout: self.timeout,
            operation_timeout: self.operation_timeout,
        }
    }
}

impl TestTemplate {
    pub fn instantiate(&self, port: Option<u16>) -> Test {
        // ----- sanity checks -----
        if self.require_communicator && port.is_none() {
            panic!("[-] Port number is required for test: {}", self.name);
        }

        if self.require_communicator && self.operation_timeout == 0 {
            panic!(
                "[-] Communicator timeout is required for test: {}",
                self.name
            );
        }

        if self.cmd_args_template.contains("{}") && port.is_none() {
            panic!("[-] Port number is required for test: {}", self.name);
        }

        let port = port.unwrap_or(0);
        let cmd_args = if self.cmd_args_template.contains("{}") {
            self.cmd_args_template.replace("{}", &port.to_string())
        } else {
            self.cmd_args_template.clone()
        };

        let mut cmd_args: Vec<String> =
            cmd_args.split_whitespace().map(|s| s.to_string()).collect();

        if self.valgrind {
            // construct valgrind arguments
            let valgrind = vec![
                String::from("valgrind"),
                String::from("--leak-check=full"),
                String::from("--tool=memcheck"),
                String::from("--show-leak-kinds=all"),
                String::from("--track-origins=yes"),
                String::from("--verbose"),
                String::from("--error-exitcode=1"),
                String::from("-v"),
                format!("--log-file=valgrind - {}", self.name),
            ];

            // chain valgrind arguments with the command arguments
            cmd_args = valgrind.into_iter().chain(cmd_args).collect();
        }

        Test {
            name: self.name.clone(),
            cmd_args,
            test: (self.test_factory)(),
            timeout: self.timeout,
            log_output: self.log_output,
            require_communicator: self.require_communicator,
            operation_timeout: self.operation_timeout,
            port,
        }
    }
}

impl Test {
    fn on_validate(&self, test_output: &ProcessOutput) -> bool {
        // panic if exercise failed to run due port already in use
        let stderr =
            String::from_utf8_lossy(&test_output.stderr).to_lowercase();
        if stderr.contains("in use")
        // address already in use
        {
            // panic!(
            //     "[-] Failed to run test: {}",
            //     stderr
            // );
            println!("[-] Failed to run test: {}", stderr);
            return false;
        }

        match &test_output.status {
            Ok(ref status) => match status.code() {
                Some(code) => {
                    if code == 0 || code == 1 {
                        return true;
                    }

                    if code == Status::Timeout as i32 {
                        println!("[-] Test timed out");
                        return false;
                    }

                    println!("[!] Test exited with status code: {}", code);
                }
                None => {
                    let signal_code = status.signal().unwrap();
                    if signal_code == libc::SIGSEGV {
                        println!(
                                "[-] Test crashed with SIGSEGV  (segmentation fault)"
                            );
                        return false;
                    } else if signal_code == libc::SIGABRT {
                        println!("[-] Test crashed with SIGABRT (core dumped)");
                        return false;
                    } else {
                        println!(
                            "[!] Test exited with signal code: {}",
                            signal_code
                        );
                    }
                }
            },

            Err(e) => {
                panic!("[-] Failed to run test: {}", e);
            }
        }
        true
    }
}

impl Test {
    pub async fn run(
        &self,
        cwd: &std::path::PathBuf,
        startup_delay: u64,
    ) -> bool {
        println!("[*] Running {} test...", self.name);

        // if no args are empty so we only do a valgrind check
        // therefore, we don't need to run the test
        if self.cmd_args.is_empty() {
            // dummy process output
            let dummy = ProcessOutput::new(
                Vec::new(),
                Vec::new(),
                Ok(ExitStatus::from_raw(0)),
            );
            return self.test.validate(&self.cmd_args, None, dummy, cwd).await;
        }

        println!("[*] Input: {}", self.cmd_args.join(" "));

        // run the exercise in a shell as a child process
        let test_proc = Arc::new(Mutex::new(
            TestSpawner::new(&self.cmd_args, cwd, startup_delay).await,
        ));

        let total_timeout = self.timeout;
        let test_output = tokio::spawn({
            let test_proc = Arc::clone(&test_proc);

            async move {
                let mut proc = test_proc.lock().await;
                return proc.wait(total_timeout).await;
            }
        });

        // optionally communicate with the process
        let communicate_output: Option<CommunicateOutput> =
            match self.require_communicator {
                true => {
                    let output = self
                        .test
                        .communicate(
                            self.operation_timeout,
                            &self.port.to_string(),
                            test_proc.lock().await.id(),
                        )
                        .await;

                    let mut output_to_log = output.output.clone();

                    if let Some(ref e) = output.error {
                        output_to_log.push(e.to_string().into_bytes());
                    }

                    let log_path =
                        cwd.join(format!("communicate - {}.txt", self.name));

                    tokio::fs::write(&log_path, output_to_log.concat())
                        .await
                        .expect(&format!(
                            "Could not write to file: {:?}",
                            log_path
                        ));

                    Some(output)
                }
                false => None,
            };

        // wait for the process to finish
        let test_output = test_output.await.expect("failed to join process");

        // log stdout and stderr
        if self.log_output {
            let log_path = cwd.join(format!("output - {}.txt", self.name));
            let mut log_file = tokio::fs::File::create(&log_path).await.expect(
                format!("Could not create file: {:?}", log_path).as_str(),
            );
            // log stdout
            log_file.write_all(&test_output.stdout).await.expect(
                format!("Could not write to file: {:?}", log_path).as_str(),
            );
            // log stderr
            log_file.write_all(&test_output.stderr).await.expect(
                format!("Could not write to file: {:?}", log_path).as_str(),
            );
        }

        let is_not_errored = self.on_validate(&test_output);
        let is_confirmed = self
            .test
            .validate(&self.cmd_args, communicate_output, test_output, cwd)
            .await;

        println!();

        is_not_errored && is_confirmed
    }
}

impl<'a> TestManager<'a> {
    pub fn new(name: &'a str, tests_dirname: &str, startup_delay: u64) -> Self {
        let tests_dir_path =
            std::env::current_dir().unwrap().join(tests_dirname);
        if !tests_dir_path.exists() {
            panic!("[-] Tests directory not found: {:?}", tests_dir_path);
        }

        Self {
            name,
            tests_dir_path,
            startup_delay,
            templates: IndexMap::new(),
            active_tests: IndexMap::new(),
        }
    }
}

impl<'a> TestManager<'a> {
    pub fn register_template(&mut self, template: TestTemplate) -> String {
        self.templates.insert(template.name.clone(), template);
        self.templates.last().unwrap().0.clone()
    }
}

impl<'a> TestManager<'a> {
    pub fn instantiate_test(&mut self, template_name: &str, port: Option<u16>) {
        let template = self.templates.get(template_name).unwrap();
        let test = template.instantiate(port);
        self.active_tests.insert(test.name.clone(), test);
    }
}

impl<'a> TestManager<'a> {
    fn remove_test(&mut self, test_name: &str) {
        self.active_tests.shift_remove(test_name);
    }
}

impl<'a> TestManager<'a> {
    pub fn reinstantiate_test(&mut self, test_name: &str, port: u16) {
        self.remove_test(test_name);
        self.instantiate_test(test_name, Some(port));
    }
}

impl<'a> TestManager<'a> {
    pub fn compile_assignment(&self, cmd: &str) -> String {
        println!("[*] Compiling assignment...");
        let res = compile(cmd, &self.tests_dir_path);

        if res == "error" {
            println!("[-] Compilation failed");
        } else if res == "warning" {
            println!("[!] Encountered warnings during compilation");
        } else {
            println!("[+] Compilation successful");
        }

        println!();
        res
    }
}

impl<'a> TestManager<'a> {
    pub fn run_tests(&self) -> Vec<(&str, bool)> {
        self.active_tests
            .iter()
            .map(|(_, test)| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let outcome = rt.block_on(
                    test.run(&self.tests_dir_path, self.startup_delay),
                );
                (test.name.as_str(), outcome)
            })
            .collect()
    }
}
