use super::run::*;
use crate::ProcessOutput;
use async_trait::async_trait;
use indexmap::IndexMap;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;

#[allow(unused_variables)]
#[async_trait]
pub trait TestAgent: Send + Sync {
    fn validate(
        &self,
        args: &Vec<String>,
        communicate_output: Option<Vec<Vec<u8>>>,
        output: ProcessOutput,
        cwd: &std::path::PathBuf,
    ) -> bool {
        unimplemented!("Must be implemented by the type")
    }

    async fn communicate(
        &self,
        read_timeout: u64,
        port: &str,
    ) -> Result<Vec<Vec<u8>>, std::io::Error> {
        unimplemented!("Must be implemented by the type")
    }
}

pub struct TestTemplateBuilder {
    // pass through new method
    name: String,
    cmd_args_template: String,
    test_factory: Option<Box<dyn Fn() -> Box<dyn TestAgent>>>,
    validate_timeout: u64,
    // validator builder attributes
    valgrind: bool,
    log_output: bool,
    // communicator builder attributes
    communicate: bool,
    communicate_timeout: u64,
}

pub struct TestTemplate {
    name: String,
    cmd_args_template: String,
    test_factory: Box<dyn Fn() -> Box<dyn TestAgent>>,
    validate_timeout: u64,
    valgrind: bool,
    log_output: bool,
    require_communicator: bool,
    communicate_output: u64,
}

pub struct Test {
    name: String,
    cmd_args: Vec<String>,
    test: Box<dyn TestAgent>,
    validator_timeout: u64,
    log_output: bool,
    require_communicator: bool,
    communicator_timeout: u64,
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
            validate_timeout: 0,
            // validator builder attributes
            valgrind: false,
            log_output: false,
            // communicator builder attributes
            communicate: false,
            communicate_timeout: 0,
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

    pub fn validate_timeout(mut self, validator_timeout: u64) -> Self {
        self.validate_timeout = validator_timeout;
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

    pub fn communicate_timeout(mut self, communicator_timeout: u64) -> Self {
        self.communicate_timeout = communicator_timeout;
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
            validate_timeout: self.validate_timeout,
            communicate_output: self.communicate_timeout,
        }
    }
}

impl TestTemplate {
    pub fn instantiate(&self, port: Option<u16>) -> Test {
        // ----- sanity checks -----
        if self.require_communicator && port.is_none() {
            panic!("[-] Port number is required for test: {}", self.name);
        }

        if self.require_communicator && self.communicate_output == 0 {
            panic!(
                "[-] Communicator timeout is required for test: {}",
                self.name
            );
        }

        if self.cmd_args_template.contains("{}") && port.is_none() {
            panic!("[-] Port number is required for test: {}", self.name);
        }

        let port = port.unwrap_or(0);
        let cmd_args = if self.require_communicator {
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
            validator_timeout: self.validate_timeout,
            log_output: self.log_output,
            require_communicator: self.require_communicator,
            communicator_timeout: self.communicate_output,
            port,
        }
    }
}

impl Test {
    pub fn run(&self, cwd: &std::path::PathBuf, startup_delay: u64) -> bool {
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
            return self.test.validate(&self.cmd_args, None, dummy, cwd);
        }

        println!("[*] Input: {}", self.cmd_args.join(" "));

        // run the exercise in a shell as a child process
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut test_proc =
            rt.block_on(TestSpawner::new(&self.cmd_args, cwd, startup_delay));

        // optionally - send/receive data
        let communicate_output: Option<Vec<Vec<u8>>> =
            self.require_communicator.then(|| {
                let output = rt
                    .block_on(self.test.communicate(
                        self.communicator_timeout,
                        &self.port.to_string(),
                    ))
                    .unwrap_or_else(|e| vec![e.to_string().into_bytes()]);

                let log_path =
                    cwd.join(format!("communicate - {}.txt", self.name));
                std::fs::write(&log_path, &output.concat()).expect(
                    format!("Could not write to file: {:?}", log_path).as_str(),
                );
                output
            });

        // wait for the process to finish
        let output = rt.block_on(test_proc.wait(self.validator_timeout));

        // log stdout and stderr
        if self.log_output {
            let log_path = cwd.join(format!("output - {}.txt", self.name));
            let mut log_file = std::fs::File::create(&log_path).expect(
                format!("Could not create file: {:?}", log_path).as_str(),
            );
            // log stdout
            log_file.write_all(&output.stdout).expect(
                format!("Could not write to file: {:?}", log_path).as_str(),
            );
            // log stderr
            log_file.write_all(&output.stderr).expect(
                format!("Could not write to file: {:?}", log_path).as_str(),
            );
        }

        // print status code
        match output.status {
            // I can stop the tests if the exit code is not 0 or 1
            Ok(ref status) => {
                log_exit_code(status);
            }
            // error could be broken pipe, etc
            Err(ref e) => {
                // println!("[-] Failed to run test: {}", e);
                panic!("[-] Failed to run test: {}", e);
            }
        }

        // validate output
        println!();
        self.test
            .validate(&self.cmd_args, communicate_output, output, cwd)
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
                let outcome =
                    test.run(&self.tests_dir_path, self.startup_delay);
                (test.name.as_str(), outcome)
            })
            .collect()
    }
}
