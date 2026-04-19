use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

pub struct DockerFacade {
    repo_root: PathBuf,
    image: String,
}

impl DockerFacade {
    pub fn start() -> Self {
        assert_docker_available();

        let repo_root = repo_root();
        let image = ensure_test_image(&repo_root);
        Self { repo_root, image }
    }

    pub fn run_example_output(&self, example: &str) -> Output {
        self.run_output([format!("/usr/local/bin/{example}")])
    }

    pub fn run_output<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut command = Command::new("docker");
        command.current_dir(&self.repo_root).arg("run").arg("--rm");
        command.arg(&self.image);
        for arg in args {
            command.arg(arg.as_ref());
        }
        command
            .output()
            .expect("docker run should succeed for facade example")
    }
}

pub fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory should have parent")
        .parent()
        .expect("workspace root should exist")
        .to_path_buf()
}

fn assert_docker_available() {
    let status = Command::new("docker")
        .arg("--version")
        .status()
        .expect("docker --version should run");
    assert!(status.success(), "docker is not available for facade tests");
}

fn ensure_test_image(repo_root: &Path) -> String {
    static IMAGE: OnceLock<String> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            let tag = std::env::var("LEMNOS_TEST_IMAGE")
                .unwrap_or_else(|_| "lemnos-facade-test:dev".to_owned());
            let skip_build = matches!(
                std::env::var("LEMNOS_SKIP_DOCKER_BUILD").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            );

            if skip_build {
                if docker_image_exists(&tag) {
                    return tag;
                }

                panic!(
                    "LEMNOS_SKIP_DOCKER_BUILD is set but docker image '{}' does not exist",
                    tag
                );
            }

            let dockerfile = repo_root.join("testing/docker/lemnos-facade.Dockerfile");
            let status = Command::new("docker")
                .current_dir(repo_root)
                .arg("build")
                .arg("--pull=false")
                .arg("-f")
                .arg(&dockerfile)
                .arg("-t")
                .arg(&tag)
                .arg(".")
                .status()
                .expect("docker build should run");
            assert!(
                status.success(),
                "docker build failed for test image '{tag}'"
            );

            tag
        })
        .clone()
}

fn docker_image_exists(tag: &str) -> bool {
    Command::new("docker")
        .arg("image")
        .arg("inspect")
        .arg(tag)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
