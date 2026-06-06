//! `team-agent` 二进制入口。

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if matches!(command.as_deref(), Some("fake-worker")) {
        let mut workspace = None;
        let mut agent_id = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--workspace" => workspace = args.next().map(std::path::PathBuf::from),
                "--agent-id" => agent_id = args.next(),
                other => return Err(anyhow::anyhow!("unknown fake-worker argument: {other}")),
            }
        }
        let workspace = workspace
            .ok_or_else(|| anyhow::anyhow!("fake-worker requires --workspace <path>"))?;
        let agent_id = agent_id
            .ok_or_else(|| anyhow::anyhow!("fake-worker requires --agent-id <id>"))?;
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        team_agent::fake_worker::run(&workspace, &agent_id, stdin.lock(), stdout.lock())
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        return Ok(());
    }
    if matches!(command.as_deref(), Some("mcp-server")) {
        let mut workspace = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--workspace" => workspace = args.next().map(std::path::PathBuf::from),
                other => return Err(anyhow::anyhow!("unknown mcp-server argument: {other}")),
            }
        }
        let workspace = workspace.unwrap_or(std::env::current_dir()?);
        team_agent::mcp_server::main(&workspace, &[])?;
        return Ok(());
    }
    let argv = std::env::args().skip(1).collect::<Vec<_>>();
    let cwd = std::env::current_dir()?;
    std::process::exit(team_agent::cli::run(&argv, &cwd).code());
}
