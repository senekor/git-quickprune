use std::{path::PathBuf, process::Command};

use anyhow::{anyhow, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Specify main branch, e.g. master or trunk.
    #[arg(short, long, default_value = "main")]
    main_branch: String,

    /// Useful for managing forks, when merging a PR may not
    /// delete the branch on your fork.
    #[arg(short = 'r', long)]
    also_delete_remote_branches: bool,

    /// By default, the editor is only opened if there is at least
    /// one branch to delete.
    #[arg(short = 'e', long)]
    always_open_editor: bool,

    #[arg(long, default_value = "origin")]
    remote: String,
}

fn main() -> Result<()> {
    let cli_args = Cli::parse();
    let main = &cli_args.main_branch;
    let remote = &cli_args.remote;

    ensure_main_branch_exists(remote, main)?;

    let mut branches_to_delete = String::new();
    let mut branches_to_keep = String::new();

    // can be empty, e.g. in detached HEAD state
    let current_branch = get_current_branch()?;

    for branch in get_local_branches()? {
        if &branch == main || branch == current_branch {
            continue;
        }

        use std::fmt::Write;

        if is_fully_merged(&format!("{remote}/{main}"), &branch)? {
            writeln!(branches_to_delete, "{}", branch)?;
        } else {
            writeln!(branches_to_keep, "# {}", branch)?;
        }
    }

    if branches_to_delete.is_empty() && !cli_args.always_open_editor {
        println!("Nothing to do. Use -e to force-open the editor.");
        return Ok(());
    }

    let staging_file_content = format!("{}{}{}", branches_to_delete, branches_to_keep, FOOTER);

    let dir = tempfile::tempdir()?;
    let staging_file_path = dir.path().join("quickprune-stage");

    write_to_staging_file(&staging_file_path, staging_file_content)?;

    // # give the user a chance to edit the list
    Command::new(select_editor())
        .arg(&staging_file_path)
        .status()?;

    let final_user_selection = std::fs::read_to_string(&staging_file_path)?;
    let branches_to_delete = final_user_selection
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::trim);

    if cli_args.also_delete_remote_branches {
        for branch in branches_to_delete.clone() {
            let mut remote_delete_handles = Vec::new();
            if should_delete_remote_branch(remote, branch)? {
                let child_handle = Command::new("git")
                    .args(["push", "--delete", remote, branch])
                    .spawn()?;
                remote_delete_handles.push(child_handle);
            }
            for mut child_handle in remote_delete_handles {
                child_handle.wait()?;
            }
        }
    }

    for branch in branches_to_delete {
        let output = Command::new("git")
            .args(["branch", "--delete", "--force", branch])
            .output()?;
        if output.status.success() {
            println!("Deleted branch '{branch}'");
        } else {
            print!(
                "Failed to delete branch '{branch}':\n{}",
                String::from_utf8(output.stderr)?
            );
        }
    }

    Ok(())
}

fn ensure_main_branch_exists(remote: &str, main: &str) -> Result<()> {
    if !Command::new("git")
        .args(["rev-parse", &format!("{remote}/{main}")])
        .output()?
        .status
        .success()
    {
        return Err(anyhow!("fatal: main branch '{main}' not found"));
    }
    Ok(())
}

fn get_current_branch() -> Result<String> {
    let mut git_output = Command::new("git")
        .args(["branch", "--show-current"])
        .output()?
        .stdout;
    git_output.pop();
    Ok(String::from_utf8(git_output)?)
}

fn get_local_branches() -> Result<Vec<String>> {
    let git_output = Command::new("git")
        .args(["branch", "--format", "%(refname:short)"])
        .output()?
        .stdout;
    Ok(String::from_utf8(git_output)?
        .lines()
        .map(|l| l.to_owned())
        .collect())
}

fn is_fully_merged(remote_main: &str, branch: &str) -> Result<bool> {
    let merge_tree_output = Command::new("git")
        .args(["merge-tree", remote_main, branch])
        .output()?;
    if !merge_tree_output.status.success() {
        // merge-tree reported conflict.
        // TODO attempt to merge with predecessors of remote_main,
        // at most until merge-base.
        // binary-search the latest commit that doesn't cause a conflict.
        // maybe binary seach is not possible, because this is:
        //
        // * commit with conflict
        // * actual squash commit, can merge with this
        // * commit with conflict
        //
        // so that means we can't use a merge conflict as evidence
        // that all later commits cannot be merged with.
        // however, we might be able to say:
        // if a commit doesn't cause a conflict, but the merge actually
        // results in a diff, then all earier commits will surely not
        // work either. (TODO try to find counter example)
        return Ok(false);
    }
    let squashed_tree_hash = String::from_utf8(merge_tree_output.stdout)?
        .trim()
        .to_owned();

    let cat_file_output = Command::new("git")
        .args(["cat-file", "-p", remote_main])
        .output()?;
    let main_tree_hash = String::from_utf8(cat_file_output.stdout)?
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("tree ")
        .to_owned();

    Ok(squashed_tree_hash == main_tree_hash)
}

static FOOTER: &str = "
# ^^^^^^^^^^^^^^^^^^^^^^^
# ! TO BE FORCE-DELETED !
#
# Review and edit the above list of branches to be force-deleted.
# To preserve a branch, remove it from the list or comment it.
# To delete a commented branch, uncomment it.";

fn write_to_staging_file(path: &PathBuf, content: String) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    write!(file, "{}", content)?;
    Ok(())
}

fn select_editor() -> String {
    match std::env::var("EDITOR") {
        Ok(editor) => editor,
        Err(_) => "vi".into(),
    }
}

/// Checks if the remote branch even exists
/// and if the local one is up to date with it.
fn should_delete_remote_branch(remote: &str, branch: &str) -> Result<bool> {
    let remote_rev = Command::new("git")
        .args(["rev-parse", &format!("{remote}/{branch}")])
        .output()?;
    if !remote_rev.status.success() {
        return Ok(false);
    }
    let remote_rev = remote_rev.stdout;
    let local_rev = Command::new("git")
        .args(["rev-parse", &format!("{remote}/{branch}")])
        .output()?
        .stdout;

    Ok(remote_rev == local_rev)
}
