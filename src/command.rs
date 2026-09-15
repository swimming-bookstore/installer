use std::process::{Output, Stdio};

use anyhow::Context;
use tokio::process::Command;

/// Run a command, returning its output, or an error carrying its stderr
#[tracing::instrument(level = "debug", skip_all, fields(command = %format!("{:?}", command.as_std())))]
pub async fn execute_command(command: &mut Command) -> anyhow::Result<Output> {
    tracing::trace!("Executing");
    let description = format!("{:?}", command.as_std());
    let output = command
        .output()
        .await
        .with_context(|| format!("Command `{description}` failed to start"))?;
    let output = successful(&description, output)?;
    tracing::trace!(
        stderr = %String::from_utf8_lossy(&output.stderr),
        stdout = %String::from_utf8_lossy(&output.stdout),
        "Command success"
    );
    Ok(output)
}

/// Run a command, returning its stdout as a `String`
pub async fn execute_command_stdout(command: &mut Command) -> anyhow::Result<String> {
    let output = execute_command(command).await?;
    String::from_utf8(output.stdout).context("Output was not valid UTF-8")
}

/// Run a command whose environment, arguments or output carry a secret
///
/// Neither a tracing span nor the error carries the command line or its output, so a
/// password in the environment or a generated key on stdout cannot reach the logs.
/// `description` is what a failure is reported as instead.
pub async fn execute_command_secret(
    command: &mut Command,
    description: &str,
) -> anyhow::Result<Output> {
    let output = command
        .output()
        .await
        .with_context(|| format!("Command `{description}` failed to start"))?;
    successful(description, output)
}

/// Run a command with `input` on its stdin
///
/// This is how a secret (SQL with a password in it, a private key) is handed to a program
/// without going through `argv`, so like [`execute_command_secret`] it logs neither the
/// command line nor the input nor the output.
pub async fn execute_command_with_stdin(
    command: &mut Command,
    input: &[u8],
    description: &str,
) -> anyhow::Result<Output> {
    use tokio::io::AsyncWriteExt;

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("Command `{description}` failed to start"))?;

    let mut stdin = child
        .stdin
        .take()
        .with_context(|| format!("Command `{description}` has no stdin"))?;
    stdin
        .write_all(input)
        .await
        .with_context(|| format!("Writing to the stdin of `{description}`"))?;
    // Closing stdin is what lets the program finish reading
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .with_context(|| format!("Waiting for `{description}`"))?;
    successful(description, output)
}

fn successful(description: &str, output: Output) -> anyhow::Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        anyhow::bail!(
            "Command `{description}` failed ({status}), stderr:\n{stderr}",
            status = output.status,
            stderr = String::from_utf8_lossy(&output.stderr),
        )
    }
}

/// Whether a command exits successfully, for probing state rather than changing it
pub async fn command_succeeds(command: &mut Command) -> bool {
    match command.output().await {
        Ok(output) => output.status.success(),
        Err(e) => {
            tracing::trace!("Command could not be started: {e}");
            false
        },
    }
}

/// A command builder which runs `program` as `user`, with that user's environment
pub fn command_as(user: &str, program: &str) -> Command {
    let mut command = Command::new("sudo");
    command
        .arg("-u")
        .arg(user)
        .arg("-H")
        .arg(program)
        .process_group(0)
        .stdin(Stdio::null());
    command
}

/// A command builder for a plain root command
pub fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.process_group(0).stdin(Stdio::null());
    command
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn true_succeeds_and_false_does_not() {
        assert!(command_succeeds(&mut command("true")).await);
        assert!(!command_succeeds(&mut command("false")).await);
    }

    #[tokio::test]
    async fn execute_command_stdout_captures_output() -> anyhow::Result<()> {
        let mut echo = command("echo");
        echo.arg("hello");
        assert_eq!(execute_command_stdout(&mut echo).await?.trim(), "hello");
        Ok(())
    }

    #[tokio::test]
    async fn failed_command_is_an_error() {
        assert!(execute_command(&mut command("false")).await.is_err());
    }
}
