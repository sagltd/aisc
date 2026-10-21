use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::ffi::CString;
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

pub(crate) fn exact_executable(command: &mut Command, argument_zero: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    const MAXIMUM: usize = 4_096;
    let program = CString::new(command.get_program().as_bytes())?;
    let arguments = std::iter::once(std::ffi::OsStr::new(argument_zero))
        .chain(command.get_args())
        .map(|value| CString::new(value.as_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(key.to_owned(), value.to_owned());
        } else {
            environment.remove(key);
        }
    }
    let environment = environment
        .into_iter()
        .map(|(key, value)| {
            let mut pair = key.as_bytes().to_vec();
            pair.push(b'=');
            pair.extend_from_slice(value.as_bytes());
            CString::new(pair)
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if arguments.len() >= MAXIMUM || environment.len() >= MAXIMUM {
        bail!("verification supports fewer than 4096 arguments and environment entries");
    }
    // SAFETY: pre_exec only fills stack pointer arrays and calls async-signal-safe execve.
    // All referenced CStrings are owned by this closure and survive through the call.
    unsafe {
        command.pre_exec(move || {
            let mut argv = [std::ptr::null(); MAXIMUM];
            let mut envp = [std::ptr::null(); MAXIMUM];
            for (index, value) in arguments.iter().enumerate() {
                argv[index] = value.as_ptr();
            }
            for (index, value) in environment.iter().enumerate() {
                envp[index] = value.as_ptr();
            }
            libc::execve(program.as_ptr(), argv.as_ptr(), envp.as_ptr());
            // Returning an error prevents execvp's implicit shell fallback on ENOEXEC.
            Err(std::io::Error::last_os_error())
        });
    }
    Ok(())
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
    let mut stdout_open = true;
    let mut stderr_open = true;
    let status = loop {
        stdout_open &= !drain(
            &mut stdout,
            &mut result.stdout,
            &mut result.stdout_omitted,
            limit,
        )?;
        stderr_open &= !drain(
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
        // Closed pipes can precede process exit briefly; avoid adding 25 ms to a fast command.
        let interval = if stdout_open || stderr_open { 25 } else { 1 };
        let remaining = timeout
            .saturating_sub(start.elapsed())
            .min(Duration::from_millis(interval));
        let milliseconds = i32::try_from(remaining.as_millis()).unwrap_or(25).max(1);
        let mut pipes = [
            libc::pollfd {
                fd: if stdout_open { stdout.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if stderr_open { stderr.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors remain owned and valid; poll only fills this stack array.
        if unsafe {
            libc::poll(
                pipes.as_mut_ptr(),
                pipes.len() as libc::nfds_t,
                milliseconds,
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error).context("could not wait for process output");
            }
        }
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
) -> Result<bool> {
    let mut buffer = [0_u8; 8_192];
    for _ in 0..64 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                let keep = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
                *omitted = omitted.saturating_add(count - keep);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => bail!("could not read process output: {error}"),
        }
    }
    Ok(false)
}
