// SPDX-License-Identifier: Apache-2.0
//! Local-owner installer harness for adapter development.
use std::{error::Error, path::PathBuf};
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: models install|remove <model> <directory>".into());
    }
    let entry = speech::catalog()
        .iter()
        .find(|m| args[1] == m.name)
        .ok_or("unknown model")?;
    let dir = PathBuf::from(&args[2]);
    if args[0] == "install" {
        let mut percent = 0;
        speech::install(entry, &dir, |p| {
            let next = p.downloaded_bytes * 100 / p.total_bytes;
            if next >= percent + 10 {
                percent = next;
                eprintln!("{}: {percent}%", p.model);
            }
        })
        .await?;
    } else if args[0] == "remove" {
        speech::remove(entry, &dir)?;
    } else {
        return Err("expected install or remove".into());
    }
    Ok(())
}
