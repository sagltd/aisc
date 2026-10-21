use anyhow::{Context, Result, bail};
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_omitted: usize,
    pub stderr_omitted: usize,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        terminate_group(self.0.id());
        let _ = self.0.wait();
    }
}

// Nonblocking pipes avoid waiting forever for descendants that inherit stdout.
pub(crate) fn execute(command: &mut Command, timeout: Duration, limit: usize) -> Result<Output> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("could not start process")?;
    let mut guarded = ChildGuard(child);
    let child = &mut guarded.0;
    let pid = child.id();
    let mut stdout = child.stdout.take().context("missing stdout pipe")?;
    let mut stderr = child.stderr.take().context("missing stderr pipe")?;
    if let Err(error) = nonblocking(&stdout).and_then(|()| nonblocking(&stderr)) {
        terminate_group(pid);
        let _ = child.wait();
        return Err(error);
    }
    let start = Instant::now();
    let mut result = Output {
        code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_omitted: 0,
        stderr_omitted: 0,
        timed_out: false,
    };
    let status = loop {
        drain(
            &mut stdout,
            &mut result.stdout,
            &mut result.stdout_omitted,
            limit,
        )?;
        drain(
            &mut stderr,
            &mut result.stderr,
            &mut result.stderr_omitted,
            limit,
        )?;
        if let Some(status) = child.try_wait().context("could not inspect process")? {
            break status;
        }
        if start.elapsed() >= timeout {
            result.timed_out = true;
            terminate_group(pid);
            break child.wait().context("could not reap timed out process")?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    terminate_group(pid);
    drain(
        &mut stdout,
        &mut result.stdout,
        &mut result.stdout_omitted,
        limit,
    )?;
    drain(
        &mut stderr,
        &mut result.stderr,
        &mut result.stderr_omitted,
        limit,
    )?;
    result.code = status.code();
    Ok(result)
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<()> {
    // SAFETY: fcntl operates on an owned, valid pipe descriptor without retaining it.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: same live descriptor, preserving all existing flags.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn terminate_group(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: child was created in its own process group, never the caller's group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn drain(
    reader: &mut impl Read,
    bytes: &mut Vec<u8>,
    omitted: &mut usize,
    limit: usize,
) -> Result<()> {
    let mut buffer = [0_u8; 8_192];
    for _ in 0..64 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                let keep = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
                *omitted = omitted.saturating_add(count - keep);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => bail!("could not read process output: {error}"),
        }
    }
    Ok(())
}
