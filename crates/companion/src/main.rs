// SPDX-License-Identifier: Apache-2.0
#[tokio::main]
async fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    if companion::cli::opened_from_explorer() {
        return std::process::ExitCode::SUCCESS;
    }
    companion::reports::install_program_panic_hook();
    let result = async {
        companion::update::recover_on_start()?;
        let factory = companion::compose::ApplicationFactory {
            options: companion::compose::ComposeOptions::from_process()?,
        };
        companion::cli::execute(companion::cli::Cli::read(), &factory).await
    }
    .await;
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // A refusal can quote a stored name.
            eprintln!("{}", companion::readable::shown(&error.to_string()));
            std::process::ExitCode::FAILURE
        }
    }
}
