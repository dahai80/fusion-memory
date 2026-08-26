//! fm: fusion-memory CLI。PRD §11.5。
//!
//! 子命令: commit / query / stats / delete / doctor。

mod cmd;
mod import_studio;
mod paths;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "fm", version, about = "fusion-memory CLI")]
struct Cli {
    /// 数据目录（默认 ~/.fusion-memory）。
    #[arg(long, env = "FM_HOME", global = true)]
    home: Option<String>,

    /// 嵌入维度（默认 64）。
    #[arg(long, default_value_t = 64, global = true)]
    dim: usize,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 写入一条交互（多轮），JSON 从 stdin 或 --file。
    Commit {
        /// session id。
        #[arg(long)]
        session: String,
        /// interaction JSON 文件；缺省读 stdin。
        #[arg(long)]
        file: Option<String>,
    },
    /// 检索记忆上下文。
    Query {
        /// 查询文本。
        #[arg(long)]
        text: String,
        /// turn 级 top_k。
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        /// token 预算。
        #[arg(long, default_value_t = 4096)]
        budget: usize,
    },
    /// 统计记忆条数。
    Stats,
    /// 软删一条记忆。
    Delete {
        /// memory id。
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// 组件健康检查。
    Doctor,
    /// 从 fusion-agent-studio memory.db 导入历史记忆。PRD §11.5。
    Import {
        /// 源库路径 (默认 ~/.fusion-agent-studio/memory.db)。
        #[arg(long)]
        source: Option<String>,
        /// 用 StubEmbedder (离线, 不连 fusion-mlx)。测试用。
        #[arg(long)]
        stub: bool,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        if let Err(e) = cmd::run(&cli).await {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    });
}
