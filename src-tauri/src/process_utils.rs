use std::{
    fs,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) fn command_output(command: &mut Command) -> Result<Output, String> {
    command.stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .map_err(|error| format!("启动进程失败：{error}"))
}
pub(super) fn command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
) -> Result<Output, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let capture_base = format!("chatx-command-{}-{nonce}", std::process::id());
    let stdout_path = std::env::temp_dir().join(format!("{capture_base}.stdout"));
    let stderr_path = std::env::temp_dir().join(format!("{capture_base}.stderr"));
    let stdout_file = fs::File::create(&stdout_path)
        .map_err(|error| format!("创建 {label} stdout 捕获文件失败：{error}"))?;
    let stderr_file = match fs::File::create(&stderr_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(format!("创建 {label} stderr 捕获文件失败：{error}"));
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(format!("启动 {label} 失败：{error}"));
        }
    };
    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = fs::read(&stdout_path).unwrap_or_default();
                let stderr = fs::read(&stderr_path).unwrap_or_default();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Err(format!(
                    "{label} 超时（{} 秒），已终止该命令。",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Err(format!("等待 {label} 失败：{error}"));
            }
        }
    }
}
