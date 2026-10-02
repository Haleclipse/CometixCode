#![cfg(unix)]

use std::{
    env, fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use super::super::SugarPath;

const CHILD_ENV: &str = "SUGAR_PATH_RELATIVE_WITHOUT_CWD";

#[test]
fn cwd_independent_relative_inputs_do_not_read_the_current_directory() {
    if let Some(doomed) = crate::utils::process_env::var_os(CHILD_ENV) {
        fs::remove_dir(&doomed).expect("remove the child's current directory");
        assert!(env::current_dir().is_err());

        assert_eq!(
            Path::new("./dist/assets/./temp/../index.js")
                .relative(Path::new("dist/./chunks/../chunks"))
                .as_os_str(),
            Path::new("../assets/index.js").as_os_str(),
        );
        assert_eq!(
            Path::new("../../dist/assets/index.js")
                .relative(Path::new("../../dist/chunks"))
                .as_os_str(),
            Path::new("../assets/index.js").as_os_str(),
        );
        assert!(
            Path::new("../../dist/assets/index.js")
                .try_relative(Path::new("../../dist/chunks"))
                .is_ok(),
        );
        assert!(
            Path::new("../../dist/assets/index.js")
                .try_relative(Path::new("../dist/chunks"))
                .is_err(),
        );

        let cwd_dependent = catch_unwind(AssertUnwindSafe(|| {
            Path::new("../../dist/assets/index.js").relative(Path::new("../dist/chunks"))
        }));
        assert!(
            cwd_dependent.is_err(),
            "unequal leading parents must retain the cwd fallback"
        );
        println!("relative child assertions passed");
        return;
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let doomed = env::temp_dir().join(format!(
        "sugar-path-relative-no-cwd-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&doomed).expect("create the child's current directory");

    let test_name = format!(
        "{}::cwd_independent_relative_inputs_do_not_read_the_current_directory",
        module_path!().split_once("::").unwrap().1
    );
    let output = Command::new(env::current_exe().expect("find the integration-test executable"))
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(CHILD_ENV, &doomed)
        .current_dir(&doomed)
        .output()
        .expect("run the integration test in a child process");

    if doomed.exists() {
        fs::remove_dir(&doomed).expect("clean up the child's current directory");
    }
    assert!(
        output.status.success(),
        "child test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("relative child assertions passed"),
        "child did not execute the intended assertions: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
