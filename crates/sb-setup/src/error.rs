use std::path::PathBuf;

/// Errors raised by setup steps.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Store(#[from] sb_store::StoreError),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed: {message}")]
    Command { command: String, message: String },
    #[error("{0}")]
    Invalid(String),
    #[error("cannot determine the user's home directory")]
    NoUserHome,
}

impl SetupError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SetupError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = SetupError> = std::result::Result<T, E>;

/// Run a command and return its stdout, mapping failures.
pub(crate) fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| SetupError::Command {
        command: program.to_string(),
        message: e.to_string(),
    })?;
    if let (Some(input), Some(mut s)) = (stdin, child.stdin.take()) {
        s.write_all(input.as_bytes())
            .map_err(|e| SetupError::Command {
                command: program.to_string(),
                message: e.to_string(),
            })?;
    }
    let out = child.wait_with_output().map_err(|e| SetupError::Command {
        command: program.to_string(),
        message: e.to_string(),
    })?;
    if !out.status.success() {
        return Err(SetupError::Command {
            command: format!("{program} {}", args.join(" ")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
