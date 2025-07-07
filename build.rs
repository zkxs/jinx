// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs, io};

const COPYRIGHT_YEAR: u32 = 2025;

fn main() -> io::Result<()> {
    let out_dir: PathBuf = env::var("OUT_DIR").expect("bad out dir?").into();
    let constants_path = out_dir.join("constants.rs");
    create_constants(constants_path.as_path())?;
    println!(
        "cargo:rustc-env=CONSTANTS_PATH={}",
        constants_path.to_str().expect("invalid unicode in constants path")
    );
    Ok(())
}

/// generate rust source to send constants into the actual build
fn create_constants<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let git_commit_hash = git_commit_hash();
    let clap_version = clap_version(&git_commit_hash);
    let discord_bot_version = discord_bot_version(&git_commit_hash);
    let user_agent = user_agent();

    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_fmt(format_args!(
        "pub const GIT_COMMIT_HASH: &str = \"{git_commit_hash}\";\n"
    ))?;
    writer.write_fmt(format_args!("pub const CLAP_VERSION: &str = \"{clap_version}\";\n"))?;
    writer.write_fmt(format_args!(
        "pub const DISCORD_BOT_VERSION: &str = \"{discord_bot_version}\";\n"
    ))?;
    writer.write_fmt(format_args!("pub const USER_AGENT: &str = \"{user_agent}\";\n"))?;
    writer.flush()
}

/// override version string displayed by clap
fn clap_version(git_commit_hash: &str) -> String {
    format!(
        "{} commit {}\\nCopyright {} jinx contributors\\nLicense: GNU AGPL v3.0 or any later version\\nWritten by: {}",
        env!("CARGO_PKG_VERSION"),
        git_commit_hash,
        COPYRIGHT_YEAR,
        env!("CARGO_PKG_AUTHORS"),
    )
}

/// String shown when via the Discord bot's `/version` command
fn discord_bot_version(git_commit_hash: &str) -> String {
    format!(
        "{} {} commit {}\\nCopyright {} jinx contributors\\nLicense: GNU AGPL v3.0 or any later version\\n{}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        git_commit_hash,
        COPYRIGHT_YEAR,
        env!("CARGO_PKG_REPOSITORY"),
    )
}

/// Read git commit hash
fn git_commit_hash() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("failed to get git commit hash");
    let untrimmed_git_commit_hash = String::from_utf8(output.stdout).expect("failed to read git commit hash as UTF-8");
    untrimmed_git_commit_hash.trim().to_string()
}

fn user_agent() -> String {
    format!(
        "{}/{} {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY")
    )
}
