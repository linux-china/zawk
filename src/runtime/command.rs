use std::io;
use std::process::{ChildStdin, Command, Stdio};

use grep_cli::{CommandError, CommandReader};

use crate::runtime::{Int, Str, StrMap};

fn prepare_command(prog: &str) -> io::Result<Command> {
    if cfg!(target_os = "windows") {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", prog]);
        Ok(cmd)
    } else {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", prog]);
        Ok(cmd)
    }
}

pub fn run_command(cmd: &str) -> Int {
    fn wrap_err(e: Option<i32>) -> Int {
        e.map(Int::from).unwrap_or(1)
    }
    fn run_command_inner(cmd: &str) -> io::Result<Int> {
        let status = prepare_command(cmd)?.status()?;
        Ok(wrap_err(status.code()))
    }
    run_command_inner(cmd).unwrap_or_else(|e| wrap_err(e.raw_os_error()))
}

pub fn run_command2(cmd: &str) -> StrMap<Str> {
    let mut map = hashbrown::HashMap::new();
    if let Ok(mut command) = prepare_command(cmd) {
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

pub fn command_for_write(bs: &[u8]) -> io::Result<ChildStdin> {
    let mut cmd = prepare_command(String::from_utf8_lossy(bs).as_ref())?;
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn()?;
    Ok(child.stdin.take().unwrap())
}

pub fn command_for_read(bs: &[u8]) -> Result<CommandReader, CommandError> {
    let mut cmd = prepare_command(String::from_utf8_lossy(bs).as_ref())?;
    CommandReader::new(&mut cmd)
}
