use crate::{SandboxPolicy, WritableRoot};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::{NamedTempFile, TempPath};

const SEATBELT_EXEC: &str = "/usr/bin/sandbox-exec";
const BASE_POLICY: &str = include_str!("seatbelt_base_policy.sbpl");
const NETWORK_POLICY: &str = include_str!("seatbelt_network_policy.sbpl");

pub struct SeatbeltInvocation {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub policy_path: TempPath,
}

pub fn build_invocation(
    command: Vec<String>,
    policy: &SandboxPolicy,
    cwd: &Path,
) -> std::io::Result<SeatbeltInvocation> {
    let mut temp_policy = NamedTempFile::new()?;
    let policy_text = policy_text(policy, cwd);
    temp_policy.write_all(policy_text.as_bytes())?;
    let temp_path = temp_policy.into_temp_path();

    let mut args = Vec::new();
    args.push("-f".to_string());
    args.push(temp_path.to_string_lossy().to_string());
    args.push("--".to_string());
    args.extend(command);

    Ok(SeatbeltInvocation {
        executable: PathBuf::from(SEATBELT_EXEC),
        args,
        policy_path: temp_path,
    })
}

fn policy_text(policy: &SandboxPolicy, cwd: &Path) -> String {
    let mut text = String::from(BASE_POLICY);

    text.push_str("(deny file-write*)\n");

    let writable_roots: Vec<WritableRoot> = policy.get_writable_roots_with_cwd(cwd);

    for root in writable_roots.iter() {
        let path = canonical_string(&root.root);
        text.push_str(&format!("(allow file-write* (subpath \"{path}\"))\n"));
        for ro in root.read_only_subpaths.iter() {
            let ro_path = canonical_string(ro);
            text.push_str(&format!("(deny file-write* (subpath \"{ro_path}\"))\n"));
        }
    }

    // Always allow writes to TMPDIR when it's already permitted by sandbox policy generation
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        let tmp_path = PathBuf::from(tmpdir);
        if writable_roots
            .iter()
            .any(|wr| tmp_path.starts_with(&wr.root))
        {
            text.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                canonical_string(&tmp_path)
            ));
        }
    }

    if policy.has_full_network_access() {
        text.push_str(NETWORK_POLICY);
    }

    text
}

fn canonical_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
