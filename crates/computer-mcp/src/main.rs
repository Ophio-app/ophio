// SPDX-License-Identifier: Apache-2.0
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // The same executable stands in for `gh`, and checks pushes, in runs
    // linked to GitHub.
    if arguments.first().is_some_and(|a| a == "gh") {
        return computer_mcp::guard::gh(arguments.split_off(1));
    }
    if arguments.first().is_some_and(|a| a == "pre-push") {
        return computer_mcp::guard::pre_push(arguments.split_off(1));
    }
    let bridge_file = match arguments.as_slice() {
        [] => None,
        [flag, path] if flag == ports::bridge::BRIDGE_FILE_ARG => {
            Some(std::path::PathBuf::from(path))
        }
        _ => {
            eprintln!("{}: unexpected arguments", computer_mcp::BINARY_NAME);
            return std::process::ExitCode::FAILURE;
        }
    };
    match computer_mcp::run_stdio(bridge_file.as_deref()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", computer_mcp::BINARY_NAME);
            std::process::ExitCode::FAILURE
        }
    }
}
