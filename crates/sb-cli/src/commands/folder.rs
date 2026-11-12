//! `sb folder`: folders that `sb sync` brings in (docs/specs/folder-sync.md,
//! ADR-0022).

use second_brain_pipeline::folders::{FolderAdd, FolderCheck, FolderInfo};
use serde_json::json;

use crate::Ctx;
use crate::cli::FolderCmd;
use crate::util::{self, exit, usage};

pub async fn run(ctx: &Ctx, cmd: FolderCmd) -> anyhow::Result<i32> {
    let p = ctx.pipeline()?;
    match cmd {
        FolderCmd::Add(a) => {
            let o = FolderAdd {
                label: a.label.clone(),
                recursive: a.no_recursive.then_some(false),
                max_depth: a.max_depth,
                include: a.include.clone(),
                exclude: a.exclude.clone(),
                initial_days: a.initial_days,
                max_files: a.max_files,
            };
            if a.initial_days.is_some_and(|d| d <= 0) {
                return Err(usage("--initial-days is a positive whole number"));
            }
            let info = p.folder_add(&a.account, &a.url, &o).await?;
            // The preview is a convenience: the folder is saved either way.
            let check = p
                .folder_check(Some(&info.account), Some(&info.folder.key), None)
                .await;
            if ctx.json {
                let (checks, problem) = match &check {
                    Ok(c) => (json!(c), None),
                    Err(e) => (json!([]), Some(e.to_string())),
                };
                ctx.out_json(
                    "sb.folder/v1",
                    json!({"folder": info, "check": checks, "check_error": problem}),
                );
            } else {
                eprintln!(
                    "Added folder {:?} to {} (key {}).",
                    info.folder.label, info.account, info.folder.key
                );
                match check {
                    Ok(c) => c.iter().for_each(print_check),
                    Err(e) => eprintln!("Could not list it yet: {e}"),
                }
                eprintln!(
                    "Run `sb sync --estimate` after a first `sb sync`, or `sb folder check` again."
                );
            }
            Ok(exit::OK)
        }
        FolderCmd::List { account } => {
            let rows = p.folder_list(account.as_deref())?;
            if ctx.json {
                ctx.out_json("sb.folder/v1", json!({ "folders": rows }));
            } else if rows.is_empty() {
                println!("No folders. Add one with `sb folder add <account> <url>`.");
            } else {
                rows.iter().for_each(print_info);
            }
            Ok(exit::OK)
        }
        FolderCmd::Remove { account, folder } => {
            let gone = p.folder_remove(&account, &folder)?;
            if ctx.json {
                ctx.out_json("sb.folder/v1", json!({"removed": gone}));
            } else {
                eprintln!(
                    "Removed folder {:?} from {account}. Its entries stay.",
                    gone.label
                );
            }
            Ok(exit::OK)
        }
        FolderCmd::Check {
            account,
            folder,
            since,
        } => {
            if folder.is_some() && account.is_none() {
                return Err(usage("name the account before the folder"));
            }
            let since = since
                .as_deref()
                .map(|s| util::parse_when(s, chrono::Utc::now()))
                .transpose()?;
            let checks = p
                .folder_check(account.as_deref(), folder.as_deref(), since)
                .await?;
            if ctx.json {
                ctx.out_json("sb.folder/v1", json!({ "check": checks }));
            } else if checks.is_empty() {
                println!("No folders to check.");
            } else {
                checks.iter().for_each(print_check);
            }
            Ok(exit::OK)
        }
    }
}

fn print_info(i: &FolderInfo) {
    println!(
        "{} / {}  [{}]  recursive: {}, depth: {}, covered since: {}, last listed: {}",
        i.account,
        i.folder.label,
        i.source_kind,
        i.folder.recursive,
        i.folder.max_depth,
        i.covered_since.as_deref().unwrap_or("-"),
        i.listed_at.as_deref().unwrap_or("never"),
    );
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn print_check(c: &FolderCheck) {
    let r = &c.report;
    eprintln!(
        "{} / {}  (files modified since {})",
        c.account,
        r.label,
        c.window_from.as_deref().unwrap_or("?")
    );
    match r.status.as_str() {
        "ok" => {
            eprintln!(
                "  in scope: {} ({} new, {} changed, {} unchanged, {} already known)",
                r.in_scope, r.new, r.changed, r.unchanged, r.duplicate
            );
            eprintln!(
                "  skipped: {} unsupported, {} excluded, {} too deep",
                r.unsupported, r.excluded, r.too_deep
            );
            if c.bytes > 0 {
                let big: Vec<String> = c
                    .largest
                    .iter()
                    .map(|(n, b)| format!("{n} ({})", human_bytes(*b)))
                    .collect();
                eprintln!(
                    "  to fetch: about {}; largest: {}",
                    human_bytes(c.bytes),
                    big.join(", ")
                );
            }
        }
        other => {
            eprintln!("  status: {other}");
            for p in &c.problems {
                eprintln!("  {p}");
            }
        }
    }
}
