//! 分层剪辑器 CLI（阶段 0）。
//!
//! 退出码：0 成功 / 1 硬约束冲突或用例失败 / 2 工程格式错误 / 3 内部错误

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use core_compile::{compile, CompileError, Status};
use core_store::{write_build, SourceSet};
use serde_json::json;

mod golden;

pub const EXIT_CONFLICT: u8 = 1;
pub const EXIT_FORMAT: u8 = 2;
pub const EXIT_INTERNAL: u8 = 3;

#[derive(Parser)]
#[command(name = "clip", version, about = "分层剪辑器 CLI（阶段 0）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 编译工程：只读源文件，只写 build/
    Compile {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 黄金用例
    Golden {
        #[command(subcommand)]
        cmd: GoldenCmd,
    },
}

#[derive(Subcommand)]
enum GoldenCmd {
    /// 运行黄金用例
    Run {
        #[arg(long)]
        case: Option<String>,
        #[arg(long, default_value = "golden")]
        root: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 用当前输出生成期望文件（必须人工逐行审阅后再提交）
    Bless {
        #[arg(long)]
        case: String,
        #[arg(long, default_value = "golden")]
        root: PathBuf,
    },
}

fn main() -> ExitCode {
    // panic 一律视为内部错误（退出码 3），而不是默认的 101
    std::panic::set_hook(Box::new(|info| {
        eprintln!("内部错误：{info}");
        std::process::exit(i32::from(EXIT_INTERNAL));
    }));
    match Cli::parse().cmd {
        Cmd::Compile { dir, json } => compile_cmd(&dir, json),
        Cmd::Golden {
            cmd: GoldenCmd::Run { case, root, json },
        } => golden::run(&root, case.as_deref(), json),
        Cmd::Golden {
            cmd: GoldenCmd::Bless { case, root },
        } => golden::bless(&root, &case),
    }
}

fn compile_cmd(dir: &Path, json: bool) -> ExitCode {
    let src = match SourceSet::read_dir(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("读取工程失败：{e}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };
    match compile(&src) {
        Ok(out) => {
            if let Err(e) = write_build(dir, &out.files) {
                eprintln!("写入 build/ 失败：{e}");
                return ExitCode::from(EXIT_INTERNAL);
            }
            let (code, status) = match out.status {
                Status::Ok => (0, "ok"),
                Status::Conflict => (EXIT_CONFLICT, "conflict"),
            };
            if json {
                let files: Vec<&String> = out.files.keys().collect();
                println!("{}", json!({"status": status, "files": files}));
            } else {
                for f in out.files.keys() {
                    println!("写入 {}", dir.join(f).display());
                }
            }
            ExitCode::from(code)
        }
        Err(CompileError::Load(errs)) => {
            if json {
                let items: Vec<_> = errs
                    .0
                    .iter()
                    .map(
                        |e| json!({"file": e.file, "at": e.at, "message": e.message, "fix": e.fix}),
                    )
                    .collect();
                println!("{}", json!({"status": "format_error", "errors": items}));
            } else {
                eprint!("{errs}");
            }
            ExitCode::from(EXIT_FORMAT)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(EXIT_INTERNAL)
        }
    }
}
