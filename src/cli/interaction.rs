use std::io::{stdin, stdout, BufRead, IsTerminal, Write};

use anyhow::{anyhow, Context};
use owo_colors::OwoColorize;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PromptChoice {
    Yes,
    No,
    Explain,
}

/// Show a plan and ask whether to go ahead with it
pub fn prompt(
    question: impl AsRef<str>,
    default: PromptChoice,
    currently_explaining: bool,
) -> anyhow::Result<PromptChoice> {
    let with_confirm = format!(
        "{question}\n\n{are_you_sure} ({yes}/{no}{maybe_explain}): ",
        question = question.as_ref(),
        are_you_sure = "Proceed?".bold(),
        yes = if default == PromptChoice::Yes {
            "[Y]es"
        } else {
            "[y]es"
        }
        .green(),
        no = if default == PromptChoice::No {
            "[N]o"
        } else {
            "[n]o"
        }
        .red(),
        maybe_explain = if currently_explaining {
            String::new()
        } else {
            format!(
                "/{}",
                if default == PromptChoice::Explain {
                    "[E]xplain"
                } else {
                    "[e]xplain"
                }
            )
        },
    );

    let mut stdout = stdout();
    stdout.write_all(with_confirm.as_bytes())?;
    stdout.flush()?;

    Ok(match read_line()?.to_lowercase().as_str() {
        "y" | "yes" => PromptChoice::Yes,
        "n" | "no" => PromptChoice::No,
        "e" | "explain" => PromptChoice::Explain,
        "" => default,
        _ => PromptChoice::No,
    })
}

pub fn read_line() -> anyhow::Result<String> {
    let stdin = stdin();
    let stdin = stdin.lock();
    let line = stdin.lines().next().transpose()?;

    line.ok_or_else(|| anyhow!("No input read from stdin"))
        .context("Reading from stdin for confirmation")
}

fn require_terminal(question: &str) -> anyhow::Result<()> {
    if stdin().is_terminal() {
        Ok(())
    } else {
        Err(anyhow!(
            "`{question}` has to be answered, but stdin is not a terminal: pass it as a flag or an environment variable instead"
        ))
    }
}

/// Ask for an existing secret, without echoing it
pub fn prompt_secret(question: &str) -> anyhow::Result<String> {
    require_terminal(question)?;

    let secret = rpassword::prompt_password(format!("{question}: "))
        .context("Reading a password from the terminal")?;
    if secret.is_empty() {
        return Err(anyhow!("The password cannot be empty"));
    }

    Ok(secret)
}

/// Ask for a new secret twice, so a typo does not become the password
pub fn prompt_new_secret(question: &str) -> anyhow::Result<String> {
    loop {
        let secret = prompt_secret(question)?;
        let confirmation = prompt_secret("Confirm")?;

        if secret == confirmation {
            return Ok(secret);
        }

        eprintln!("{}", "The passwords do not match, try again.".red());
    }
}

pub fn clean_exit_with_message(message: impl AsRef<str>) -> ! {
    eprintln!("{}", message.as_ref());
    std::process::exit(0)
}
