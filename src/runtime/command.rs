use std::cell::RefCell;
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::rc::Weak;

use grep_cli::{CommandError, CommandReader};
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
    fn wrap_err(e: Option<i32>) -> Int {
        e.map(Int::from).unwrap_or(1)
    }
    fn run_command_inner(cmd: &str) -> io::Result<Int> {
        let status = prepare_command(cmd, command_env().as_ref())?.status()?;
        Ok(wrap_err(status.code()))
    }
    run_command_inner(cmd).unwrap_or_else(|e| wrap_err(e.raw_os_error()))
}

pub(crate) fn run_command2<'b>(cmd: &str) -> StrMap<'b, Str<'b>> {
    let mut map = hashbrown::HashMap::new();
    if let Ok(mut command) = prepare_command(cmd, command_env().as_ref()) {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Ok(output) = command.output() {
            map.insert(Str::from("code"), Str::from(output.status.code().map(|i| i.to_string()).unwrap_or_else(|| "0".to_owned())));
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

pub fn command_for_read(bs: &[u8]) -> Result<CommandReader, CommandError> {
    let mut cmd = prepare_command(String::from_utf8_lossy(bs).as_ref(), command_env().as_ref())?;
    CommandReader::new(&mut cmd)
}
