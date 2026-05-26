use clap::{Parser, Subcommand};

/// Anthropic <-> Kiro API 客户端
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// 配置文件路径
    #[arg(short, long)]
    pub config: Option<String>,

    /// 凭证文件路径
    #[arg(long)]
    pub credentials: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// 子命令
#[derive(Subcommand, Debug)]
pub enum Command {
    /// 交互式生成最小可运行的 config.json（host/port/apiKey/adminApiKey）
    ///
    /// 仅写入用户回答过的字段，其它字段全部走 Config 默认值。
    /// adminApiKey 留空 → Admin API + Admin UI 不启用。
    Init {
        /// 强制覆盖已存在的配置文件（默认遇到已存在文件会确认）
        #[arg(long)]
        force: bool,
    },
}
