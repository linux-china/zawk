use std::cell::RefCell;
use std::io;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::rc::Weak;

use hashbrown::HashMap;

use crate::runtime::{Int, Str, StrMap};

/// The environment passed to child processes: the contents of ENVIRON.
pub(crate) type CommandEnv = Vec<(String, String)>;

thread_local! {
    // The ENVIRON map of the program running on this thread. It is held weakly so that it is
    // freed along with the program's variables.
    static ENVIRON: RefCell<Option<Weak<RefCell<HashMap<Str<'static>, Str<'static>>>>>> =
        const { RefCell::new(None) };
}

/// Use `environ` as the environment of commands started from this thread, so that (as in gawk)
/// changes to ENVIRON are passed to child processes.
pub(crate) fn register_environ(environ: &StrMap<'_, Str<'_>>) {
    let weak = std::rc::Rc::downgrade(&environ.0);
    // SAFETY: the map is only accessed through `upgrade` while it is still alive, and only to
    // copy its contents out; the lifetime only marks string literals from the program text.
    let weak = unsafe {
        std::mem::transmute::<
            Weak<RefCell<HashMap<Str<'_>, Str<'_>>>>,
            Weak<RefCell<HashMap<Str<'static>, Str<'static>>>>,
        >(weak)
    };
    ENVIRON.with(|e| *e.borrow_mut() = Some(weak));
}

/// A snapshot of ENVIRON for the current thread, or `None` if no ENVIRON is registered (in which
/// case commands inherit the process environment).
pub(crate) fn command_env() -> Option<CommandEnv> {
    let map = ENVIRON.with(|e| e.borrow().as_ref().and_then(Weak::upgrade))?;
    let map = map.borrow();
    let lossy = |s: &Str| s.with_bytes(|bs| String::from_utf8_lossy(bs).into_owned());
    Some(map.iter().map(|(k, v)| (lossy(k), lossy(v))).collect())
}

fn prepare_command(prog: &str, env: Option<&CommandEnv>) -> io::Result<Command> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", prog]);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", prog]);
        cmd
    };
    if let Some(env) = env {
        cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    Ok(cmd)
}

pub fn run_command(cmd: &str) -> Int {
    fn run_command_inner(cmd: &str) -> io::Result<Int> {
        let status = prepare_command(cmd, command_env().as_ref())?.status()?;
        Ok(exit_status_code(status))
    }
    run_command_inner(cmd).unwrap_or_else(|e| e.raw_os_error().map(Int::from).unwrap_or(1))
}

pub(crate) fn run_command2<'b>(cmd: &str) -> StrMap<'b, Str<'b>> {
    let mut map = hashbrown::HashMap::new();
    if let Ok(mut command) = prepare_command(cmd, command_env().as_ref()) {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Ok(output) = command.output() {
            map.insert(Str::from("code"), Str::from(exit_status_code(output.status).to_string()));
            if !output.stdout.is_empty() {
                map.insert(Str::from("stdout"), Str::from(String::from_utf8_lossy(&output.stdout).to_string()));
            }
            if !output.stderr.is_empty() {
                map.insert(Str::from("stderr"), Str::from(String::from_utf8_lossy(&output.stderr).to_string()));
            }
        } else {
            map.insert(Str::from("stderr"), Str::from("Failed to execute command"));
        }
    } else {
        map.insert(Str::from("stderr"), Str::from("Failed to construct command line"));
    }
    StrMap::from(map)
}

/// Spawn `bs` as a shell command with a piped stdin. The caller owns the child and is responsible
/// for waiting on it (see `writers::CmdWriter`). `env` is the command's environment (see
/// `command_env`), as it is spawned on a writer thread.
pub fn command_for_write(bs: &[u8], env: Option<&CommandEnv>) -> io::Result<Child> {
    let mut cmd = prepare_command(String::from_utf8_lossy(bs).as_ref(), env)?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn()
}

/// The value of awk's `close` for a command: its exit status, or 256 plus the signal number if
/// it was killed by a signal (as in gawk).
pub(crate) fn exit_status_code(status: ExitStatus) -> Int {
    if let Some(code) = status.code() {
        return code as Int;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 256 + sig as Int;
        }
    }
    -1
}

/// The output of a command run by `cmd | getline`. Unlike a plain `ChildStdout`, it keeps the
/// child so that `close` can report its exit status.
pub(crate) struct CommandReader {
    child: Child,
    stdout: Option<ChildStdout>,
}

impl CommandReader {
    /// Close the command's output and wait for it to exit, returning the value of awk's `close`
    /// (see `exit_status_code`). A command that has not written all of its output yet is
    /// typically killed by SIGPIPE, as in gawk.
    pub(crate) fn close(&mut self) -> Int {
        drop(self.stdout.take());
        match self.child.wait() {
            Ok(status) => exit_status_code(status),
            Err(_) => -1,
        }
    }
}

impl io::Read for CommandReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.stdout {
            Some(stdout) => stdout.read(buf),
            None => Ok(0),
        }
    }
}

impl Drop for CommandReader {
    fn drop(&mut self) {
        // Reap the child so it does not linger as a zombie.
        if self.stdout.is_some() {
            self.close();
        }
    }
}

pub(crate) fn command_for_read(bs: &[u8]) -> io::Result<CommandReader> {
    let mut cmd = prepare_command(String::from_utf8_lossy(bs).as_ref(), command_env().as_ref())?;
    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take();
    Ok(CommandReader { child, stdout })
}
