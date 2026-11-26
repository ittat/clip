//! 分层剪辑器 CLI（阶段 0）。
//!
//! 退出码：0 成功 / 1 硬约束冲突、用例失败或有差异 / 2 工程格式错误 / 3 内部错误
//!
//! 命令：compile · validate · explain · diff · determinism · golden run/bless ·
//! freeze · unfreeze · lock accept/check

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use core_compile::{compile, CompileError, Status};
use core_store::{remove_source, write_build, write_source, SourceSet, SOLVER_LOCK};
use serde_json::json;

mod diff;
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
    /// 生成一个能直接编译的空工程骨架
    Init { dir: PathBuf },
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
    /// 只加载和校验工程（引用、格式），不编译
    Validate {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 用人能读的方式说明求解结果：谁被压缩、谁被吸附、为什么
    Explain {
        dir: PathBuf,
        /// 只展开这个段落
        #[arg(long = "ref")]
        section: Option<String>,
    },
    /// 比较两个工程的编译结果（退出码 0 相同 / 1 不同）
    Diff {
        a: PathBuf,
        b: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 对任意工程检查幂等、顺序无关、局部性
    Determinism {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// 冻结段落：把它当前的片段存成 frozen/<段落>.json，之后编译原样输出
    Freeze { dir: PathBuf, section: String },
    /// 解冻段落：删除 frozen/<段落>.json
    Unfreeze { dir: PathBuf, section: String },
    /// 求解器版本锁
    Lock {
        #[command(subcommand)]
        cmd: LockCmd,
    },
}

#[derive(Subcommand)]
enum LockCmd {
    /// 用当前结果写 locks/solver.lock.json（确认求解器升级带来的变化）
    Accept { dir: PathBuf },
    /// 检查锁：求解器版本或区结果和锁不一致时退出码 1
    Check { dir: PathBuf },
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
        Cmd::Init { dir } => init_cmd(&dir),
        Cmd::Validate { dir, json } => validate_cmd(&dir, json),
        Cmd::Explain { dir, section } => explain_cmd(&dir, section.as_deref()),
        Cmd::Diff { a, b, json } => diff::run(&a, &b, json),
        Cmd::Determinism { dir, json } => golden::determinism(&dir, json),
        Cmd::Freeze { dir, section } => freeze_cmd(&dir, &section),
        Cmd::Unfreeze { dir, section } => unfreeze_cmd(&dir, &section),
        Cmd::Lock {
            cmd: LockCmd::Accept { dir },
        } => lock_accept(&dir),
        Cmd::Lock {
            cmd: LockCmd::Check { dir },
        } => lock_check(&dir),
    }
}

fn read_src(dir: &Path) -> Result<SourceSet, ExitCode> {
    SourceSet::read_dir(dir).map_err(|e| {
        eprintln!("读取工程失败：{e}");
        ExitCode::from(EXIT_INTERNAL)
    })
}

fn compile_error(e: CompileError) -> ExitCode {
    match e {
        CompileError::Load(errs) => {
            eprint!("{errs}");
            ExitCode::from(EXIT_FORMAT)
        }
        CompileError::Usage(m) => {
            eprintln!("{m}");
            ExitCode::from(EXIT_FORMAT)
        }
        CompileError::Internal(m) => {
            eprintln!("内部错误：{m}");
            ExitCode::from(EXIT_INTERNAL)
        }
    }
}

/// `clip init` 写入的文件：一个段落、一个只有停顿的节拍，能直接编译通过。
pub const INIT_FILES: &[(&str, &str)] = &[
    (
        "project.yaml",
        "format: 1\nfps: 25            # 整数，或 \"30000/1001\"\nsample_rate: 48000\nsolver: \"0.0.1\"\n",
    ),
    (
        "intent.yaml",
        "goal: 新工程\nduration: {target: 1s, tol: 10%}\nmust: []\nforbid: []\n",
    ),
    (
        "structure.yaml",
        "sequence: [S1]\nsections:\n  S1:\n    role: opening\n    beats: [S1.b1]\n",
    ),
    (
        "content.yaml",
        "beats:\n  S1.b1:\n    pause: {after: 1s, flex: 0.5s..2s}\n",
    ),
    ("narration.yaml", "{}\n"),
    ("voices.yaml", "{}\n"),
    ("overrides.yaml", "{}\n"),
    ("rules.yaml", "{}\n"),
    ("queue.yaml", "items: {}\n"),
    ("media/index.yaml", "shots: {}\nevents: {}\nmusic: {}\nmarks: {}\n"),
    ("locks/tts.lock.json", "{\n  \"entries\": {},\n  \"format\": 1\n}\n"),
];

fn init_cmd(dir: &Path) -> ExitCode {
    if dir.join("project.yaml").exists() {
        eprintln!(
            "{} 已经是一个工程（有 project.yaml），不覆盖",
            dir.display()
        );
        return ExitCode::from(EXIT_FORMAT);
    }
    for (name, text) in INIT_FILES {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("创建目录失败：{e}");
                return ExitCode::from(EXIT_INTERNAL);
            }
        }
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("写入 {} 失败：{e}", path.display());
            return ExitCode::from(EXIT_INTERNAL);
        }
    }
    println!(
        "已在 {} 生成工程骨架。下一步：clip compile {}",
        dir.display(),
        dir.display()
    );
    ExitCode::SUCCESS
}

fn validate_cmd(dir: &Path, json: bool) -> ExitCode {
    let src = match read_src(dir) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match core_store::load(&src) {
        Ok(p) => {
            let counts = json!({
                "sections": p.sections.len(),
                "beats": p.beats.len(),
                "narration": p.narration.len(),
                "overrides": p.overrides.len(),
                "rules": p.rules.len(),
                "frozen": p.frozen.len(),
            });
            if json {
                println!("{}", json!({"status": "ok", "counts": counts}));
            } else {
                println!(
                    "工程有效：{} 个段落、{} 个节拍、{} 句旁白、{} 个补丁、{} 条规则、{} 个冻结段",
                    p.sections.len(),
                    p.beats.len(),
                    p.narration.len(),
                    p.overrides.len(),
                    p.rules.len(),
                    p.frozen.len()
                );
            }
            ExitCode::SUCCESS
        }
        Err(errs) => {
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
                eprintln!("{} 个错误", errs.0.len());
            }
            ExitCode::from(EXIT_FORMAT)
        }
    }
}

fn explain_cmd(dir: &Path, section: Option<&str>) -> ExitCode {
    let src = match read_src(dir) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let out = match compile(&src) {
        Ok(o) => o,
        Err(e) => return compile_error(e),
    };
    let Some(bytes) = out.files.get(core_compile::REPORT) else {
        eprintln!("内部错误：编译没有产出求解报告");
        return ExitCode::from(EXIT_INTERNAL);
    };
    let report: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("内部错误：读不到求解报告：{e}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };
    if let Some(s) = section {
        if report["sections"][s].is_null() {
            eprintln!("段落 {s} 不存在");
            return ExitCode::from(EXIT_FORMAT);
        }
    }
    print!("{}", core_compile::explain(&report, section));
    ExitCode::SUCCESS
}

fn freeze_cmd(dir: &Path, section: &str) -> ExitCode {
    let src = match read_src(dir) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match core_compile::freeze(&src, section) {
        Ok((name, bytes)) => match write_source(dir, &name, &bytes) {
            Ok(()) => {
                println!("已冻结 {section}：写入 {}", dir.join(&name).display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("写入 {name} 失败：{e}");
                ExitCode::from(EXIT_INTERNAL)
            }
        },
        Err(e) => compile_error(e),
    }
}

fn unfreeze_cmd(dir: &Path, section: &str) -> ExitCode {
    if let Err(e) = core_model::SectionId::parse(section) {
        eprintln!("{e}");
        return ExitCode::from(EXIT_FORMAT);
    }
    let name = format!("frozen/{section}.json");
    match remove_source(dir, &name) {
        Ok(()) => {
            println!("已解冻 {section}");
            ExitCode::SUCCESS
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{section} 没有冻结");
            ExitCode::from(EXIT_FORMAT)
        }
        Err(e) => {
            eprintln!("删除 {name} 失败：{e}");
            ExitCode::from(EXIT_INTERNAL)
        }
    }
}

fn lock_accept(dir: &Path) -> ExitCode {
    let src = match read_src(dir) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match core_compile::lock_file(&src) {
        Ok(bytes) => match write_source(dir, SOLVER_LOCK, &bytes) {
            Ok(()) => {
                println!("已更新 {}", dir.join(SOLVER_LOCK).display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("写入锁文件失败：{e}");
                ExitCode::from(EXIT_INTERNAL)
            }
        },
        Err(e) => compile_error(e),
    }
}

fn lock_check(dir: &Path) -> ExitCode {
    let src = match read_src(dir) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let out = match compile(&src) {
        Ok(o) => o,
        Err(e) => return compile_error(e),
    };
    let Some(bytes) = out.files.get(core_compile::REPORT) else {
        eprintln!("内部错误：编译没有产出求解报告");
        return ExitCode::from(EXIT_INTERNAL);
    };
    let report: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("内部错误：读不到求解报告：{e}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };
    let lock = &report["solver_lock"];
    if lock.is_null() {
        println!("没有锁文件。运行 clip lock accept 生成");
        return ExitCode::from(EXIT_CONFLICT);
    }
    let changed: Vec<&str> = lock["zones_changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let version_ok = lock["version_matches"].as_bool() == Some(true);
    if version_ok && changed.is_empty() {
        println!("锁一致：求解器 {}", lock["solver"]);
        ExitCode::SUCCESS
    } else {
        if !version_ok {
            println!(
                "求解器版本：锁里是 {}，当前是 {}",
                lock["solver"], report["solver"]
            );
        }
        if !changed.is_empty() {
            println!("结果变化的区：{}", changed.join("、"));
        }
        ExitCode::from(EXIT_CONFLICT)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_skeleton_compiles() {
        let mut src = SourceSet::default();
        for (name, text) in INIT_FILES {
            src.insert(name, (*text).to_string());
        }
        let out = compile(&src).expect("骨架必须能编译");
        assert_eq!(out.status, Status::Ok);
    }
}
