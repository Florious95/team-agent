//! ---
//! purpose: 把 leader 手写的三件判据夹具迁成 cargo test 能跑到的测试基建
//! contract:
//!   provides:
//!     - name: assert_probe_red
//!       what: 跑 probe，要求修复前非零且不是 exit 2；已绿则 Fail，exit 2 保持 Unjudgeable
//!     - name: assert_probe_two_sided
//!       what: 先要求坏状态红，再跑 make_green，再要求好状态绿；缺任一半则 Fail，exit 2 不折态
//!     - name: check_brief
//!       what: lint 任务书是否含既有四条硬条款；读不到则 Unjudgeable，缺条款则 Fail
//!   depends:
//!     - sh（只用来跑调用方提供的 probe / make_green 脚本）
//! boundary:
//!   - 不删除、不改写 .team/probes 下原三件；去留由审查席裁定
//!   - 不发明第四个退出码；超预算与不适用都保持 exit 2（不可判），不折进 0/1
//!   - 不写产品路径判据，只夹住「判据本身有没有分辨力」
//!   - ⛔ 「工具自己坏了」不许叫 Unjudgeable：sh 起不来 / 收不到输出 / 子进程被信号打死
//!     一律走 GateVerdict::ToolFailure，与探针自报 exit 2 是两个值
//! maturity: wired
//! ---

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 夹具四态里实际会出口的三值（超预算与不适用都走 Unjudgeable，exit 2，与原脚本一致），
/// 外加一个**不属于四态**的出口：ToolFailure —— 量具自己坏了，不是被测对象不可判。
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateVerdict {
    Pass,
    Fail,
    Unjudgeable,
    /// 量具坏了：sh 起不来 / 输出收不回来 / 子进程被信号打死。
    /// ⛔ 绝不塌进 Unjudgeable——那样「探针真报不可判」与「探针根本没跑」就没有分辨力。
    ToolFailure(ShFailure),
}

/// run_sh 的失败分类。Debug 打出来即为一眼可辨的失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShFailure {
    /// 起不了 sh（CI 上没有 sh / PATH 不对 / 权限）——⛔ 与探针 exit 2 无关。
    Spawn { script: String, error: String },
    /// sh 起来了但收不回 stdout/stderr。
    OutputCapture { script: String, error: String },
    /// 子进程没有退出码：被信号打死（unix 下带信号号）。
    KilledBySignal { script: String, signal: Option<i32> },
}

impl GateVerdict {
    fn from_probe_exit(code: i32) -> ProbeOutcome {
        match code {
            0 => ProbeOutcome::Green,
            2 => ProbeOutcome::Unjudgeable,
            _ => ProbeOutcome::Red,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Green,
    Red,
    Unjudgeable,
}

/// ---
/// purpose: 因果性夹具——probe 必须在修复前判红
/// params:
///   probe: 被夹的判据脚本路径
/// returns: Pass=见到红；Fail=修复前已绿；Unjudgeable=缺文件或 probe 自报 exit 2；
///          ToolFailure=sh 根本没跑起来/输出丢了/被信号打死（量具坏，非不可判）
/// ---
fn assert_probe_red(probe: &Path) -> GateVerdict {
    if !probe.is_file() {
        return GateVerdict::Unjudgeable;
    }
    let log = probe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("RED-EVIDENCE.log");
    let _ = fs::write(
        &log,
        format!("#PROBE {}\n#---- 原始输出 ----\n", probe.display()),
    );
    match run_sh(probe, &log) {
        Err(failure) => GateVerdict::ToolFailure(failure),
        Ok(0) => GateVerdict::Fail,
        Ok(2) => GateVerdict::Unjudgeable,
        Ok(_) => GateVerdict::Pass,
    }
}

/// ---
/// purpose: 两头夹住——坏状态必须红，好状态必须绿
/// params:
///   probe: 被夹的判据脚本
///   make_green: 只许动隔离夹具的阳性对照
/// returns: 两半都成立才 Pass；因果性或可满足性缺一则 Fail；缺脚本 / 对照失败 / exit 2 为 Unjudgeable；
///          任一次 sh 没跑起来则 ToolFailure（量具坏，非不可判）
/// ---
fn assert_probe_two_sided(probe: &Path, make_green: &Path) -> GateVerdict {
    if !probe.is_file() || !make_green.is_file() {
        return GateVerdict::Unjudgeable;
    }
    let log = probe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("TWO-SIDED-EVIDENCE.log");
    let _ = fs::write(
        &log,
        format!(
            "#PROBE     {}\n#MAKEGREEN {}\n",
            probe.display(),
            make_green.display()
        ),
    );

    match run_sh(probe, &log) {
        Err(failure) => return GateVerdict::ToolFailure(failure),
        Ok(0) => return GateVerdict::Fail,
        Ok(2) => return GateVerdict::Unjudgeable,
        Ok(_) => {}
    }

    match run_sh(make_green, &log) {
        Err(failure) => return GateVerdict::ToolFailure(failure),
        Ok(0) => {}
        // 阳性对照自己没做成（脚本跑了但没成功）——这是真·不可判，不是量具坏。
        Ok(_) => return GateVerdict::Unjudgeable,
    }

    match run_sh(probe, &log) {
        Err(failure) => GateVerdict::ToolFailure(failure),
        Ok(0) => GateVerdict::Pass,
        Ok(2) => GateVerdict::Unjudgeable,
        Ok(_) => GateVerdict::Fail,
    }
}

const BRIEF_REQUIRED: &[(&str, &[&str])] = &[
    (
        "report_result 不传 task_id",
        &["不要传 task_id", "不传 task_id", "留空走框架默认归属"],
    ),
    (
        "禁止改判据",
        &["不许改判据", "不得修改判据", "判据由", "不许动判据"],
    ),
    ("产物落地路径", &[".team/nodes/"]),
    (
        "验收判据是行为自证",
        &["行为自证", "退出码", "expected_exit_code"],
    ),
];

/// ---
/// purpose: 任务书结构 lint，只挡「缺了必失败」的已知条款
/// params:
///   task_md: TASK.md 路径
/// returns: 条款齐全 Pass；缺条款或过短 Fail；读不到 Unjudgeable
/// ---
fn check_brief(task_md: &Path) -> GateVerdict {
    let text = match fs::read_to_string(task_md) {
        Ok(t) => t,
        Err(_) => return GateVerdict::Unjudgeable,
    };
    if text.trim().len() < 200 {
        return GateVerdict::Fail;
    }
    let missing = BRIEF_REQUIRED
        .iter()
        .any(|(_name, keys)| !keys.iter().any(|k| text.contains(k)));
    if missing {
        GateVerdict::Fail
    } else {
        GateVerdict::Pass
    }
}

/// ---
/// purpose: 跑一个 sh 脚本并把失败原因分类带回来
/// returns: Ok(退出码)；Err(ShFailure) 区分 起不来 / 收不到输出 / 被信号打死
/// ---
/// ⛔ 不许再用 `.ok()?` 把 io::Error 吞成 None——那会让「量具坏」与「exit 2」同值。
/// 拆成 spawn + wait_with_output 两步，正是为了让前两类可分。
fn run_sh(script: &Path, log: &Path) -> Result<i32, ShFailure> {
    let name = script.display().to_string();
    let child = Command::new(shell_for_this_call())
        .arg(script)
        // 钉死子 shell 自己的 PATH：它内部再解析裸名外部命令（如 touch）时不能继承
        // 父进程当次可能已被并发测试改写的 PATH（绝对路径只保证这一次 spawn 本身不查 PATH）。
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ShFailure::Spawn {
            script: name.clone(),
            error: error.to_string(),
        })?;
    let output = child
        .wait_with_output()
        .map_err(|error| ShFailure::OutputCapture {
            script: name.clone(),
            error: error.to_string(),
        })?;
    let mut buf = fs::read(log).unwrap_or_default();
    buf.extend_from_slice(&output.stdout);
    buf.extend_from_slice(&output.stderr);
    let _ = fs::write(log, buf);
    output
        .status
        .code()
        .ok_or_else(|| ShFailure::KilledBySignal {
            script: name,
            signal: termination_signal(&output.status),
        })
}

/// 一个**绝对路径**的不存在解释器。绝对路径 ⇒ execvp 不查 PATH，必然 ENOENT。
/// ⛔ 不用「unset PATH」那一招：unix 下 PATH 未设时 execvp 回落 confstr(_CS_PATH)，
/// 仍能找到 /bin/sh，破坏是无效的。
const UNSPAWNABLE_SHELL: &str = "/nonexistent-gate-fixtures/no-such-shell";

thread_local! {
    /// 只在**本线程**生效的量具破坏开关：列出「第几次 run_sh 调用该用坏解释器」。
    /// 线程局部 ⇒ 不碰 PATH、不碰任何进程级环境，绝不外溢到并发跑的别的测试。
    static BREAK_SH_ON_CALLS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static SH_CALL_SEQ: Cell<usize> = const { Cell::new(0) };
}

fn shell_for_this_call() -> &'static str {
    let n = SH_CALL_SEQ.with(|seq| {
        let n = seq.get();
        seq.set(n + 1);
        n
    });
    if BREAK_SH_ON_CALLS.with(|calls| calls.borrow().contains(&n)) {
        UNSPAWNABLE_SHELL
    } else {
        // 绝对路径 ⇒ execve 不查 PATH：不与并发跑的、会原地改写进程级 PATH 的其它测试赛跑。
        "/bin/sh"
    }
}

/// 在本线程内令第 `calls` 次 run_sh 调用的解释器起不来，跑完立刻复位。
/// 用于在**健康环境**下主动造出「量具失效」，不依赖运行环境恰好坏掉。
fn with_broken_sh_on<T>(calls: &[usize], body: impl FnOnce() -> T) -> T {
    SH_CALL_SEQ.with(|seq| seq.set(0));
    BREAK_SH_ON_CALLS.with(|c| *c.borrow_mut() = calls.to_vec());
    let out = body();
    BREAK_SH_ON_CALLS.with(|c| c.borrow_mut().clear());
    SH_CALL_SEQ.with(|seq| seq.set(0));
    out
}

#[cfg(unix)]
fn termination_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn termination_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn scratch_dir() -> PathBuf {
    // scratch 根必须可移植：CI(ubuntu) 上 `/Volumes/...` 不存在且 `/` 不可写，
    // create_dir_all 直接 errno 13。⛔ 不硬编码任何绝对路径；默认走标准临时目录
    // (`std::env::temp_dir()`，尊重 TMPDIR)，`TEAM_AGENT_TEST_TMP` 仍可覆盖。
    let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!(
        "gate-fixtures-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create gate-fixtures scratch");
    dir
}

fn write_sh(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).expect("write fixture script");
    path
}

fn valid_brief() -> String {
    // 远超 200 字符，四条硬条款各出现至少一处。故意弄坏测试会删掉其中一条。
    format!(
        "本任务书给下一棒执行。产物必须落在 .team/nodes/gate-governance/ 下。\n\
         调 report_result 不要传 task_id，留空走框架默认归属。\n\
         不许改判据。探针与夹具实现席都不得修改。\n\
         验收必须是行为自证：看退出码 / expected_exit_code，不要看席位自报。\n\
         填充段落：{}。\n",
        "约束坐标与隔离环境说明。".repeat(8)
    )
}

#[test]
fn assert_probe_red_passes_on_a_genuinely_red_probe() {
    let dir = scratch_dir();
    let probe = write_sh(&dir, "probe.sh", "exit 1");
    assert_eq!(assert_probe_red(&probe), GateVerdict::Pass);
}

#[test]
fn assert_probe_red_fails_when_subject_is_already_green() {
    let dir = scratch_dir();
    let good = write_sh(&dir, "ok.sh", "exit 1");
    assert_eq!(assert_probe_red(&good), GateVerdict::Pass);
    let broken = write_sh(&dir, "already-green.sh", "exit 0");
    assert_eq!(assert_probe_red(&broken), GateVerdict::Fail);
}

#[test]
fn assert_probe_red_keeps_exit2_unjudgeable_not_red_or_green() {
    let dir = scratch_dir();
    let probe = write_sh(&dir, "probe.sh", "exit 2");
    let v = assert_probe_red(&probe);
    assert_eq!(v, GateVerdict::Unjudgeable);
    assert_ne!(v, GateVerdict::Fail);
    assert_ne!(v, GateVerdict::Pass);
}

#[test]
fn assert_probe_red_missing_probe_is_unjudgeable() {
    let dir = scratch_dir();
    assert_eq!(
        assert_probe_red(&dir.join("nope.sh")),
        GateVerdict::Unjudgeable
    );
}

#[test]
fn assert_probe_two_sided_passes_when_both_halves_hold() {
    let dir = scratch_dir();
    let flag = dir.join("fixed");
    let probe = write_sh(
        &dir,
        "probe.sh",
        &format!(
            "if [ -f '{}' ]; then exit 0; else exit 1; fi",
            flag.display()
        ),
    );
    let mk = write_sh(
        &dir,
        "make_green.sh",
        &format!("touch '{}'", flag.display()),
    );
    assert_eq!(assert_probe_two_sided(&probe, &mk), GateVerdict::Pass);
}

#[test]
fn assert_probe_two_sided_fails_when_probe_is_green_before_fix() {
    let dir = scratch_dir();
    let probe = write_sh(&dir, "probe.sh", "exit 0");
    let mk = write_sh(&dir, "make_green.sh", "exit 0");
    assert_eq!(assert_probe_two_sided(&probe, &mk), GateVerdict::Fail);
}

#[test]
fn assert_probe_two_sided_fails_when_probe_can_never_go_green() {
    let dir = scratch_dir();
    let probe = write_sh(&dir, "probe.sh", "exit 1");
    let mk = write_sh(&dir, "make_green.sh", "exit 0");
    assert_eq!(assert_probe_two_sided(&probe, &mk), GateVerdict::Fail);
}

#[test]
fn assert_probe_two_sided_keeps_exit2_unjudgeable_on_either_half() {
    let dir = scratch_dir();
    let always2 = write_sh(&dir, "u.sh", "exit 2");
    let mk = write_sh(&dir, "make_green.sh", "exit 0");
    let v1 = assert_probe_two_sided(&always2, &mk);
    assert_eq!(v1, GateVerdict::Unjudgeable);
    assert_ne!(v1, GateVerdict::Fail);
    assert_ne!(v1, GateVerdict::Pass);

    let flag = dir.join("fixed");
    let probe = write_sh(
        &dir,
        "probe.sh",
        &format!(
            "if [ -f '{}' ]; then exit 2; else exit 1; fi",
            flag.display()
        ),
    );
    let mk2 = write_sh(&dir, "mk2.sh", &format!("touch '{}'", flag.display()));
    let v2 = assert_probe_two_sided(&probe, &mk2);
    assert_eq!(v2, GateVerdict::Unjudgeable);
}

#[test]
fn assert_probe_two_sided_make_green_failure_is_unjudgeable() {
    let dir = scratch_dir();
    let probe = write_sh(&dir, "probe.sh", "exit 1");
    let mk = write_sh(&dir, "make_green.sh", "exit 1");
    assert_eq!(
        assert_probe_two_sided(&probe, &mk),
        GateVerdict::Unjudgeable
    );
}

#[test]
fn check_brief_passes_when_all_required_clauses_present() {
    let dir = scratch_dir();
    let path = dir.join("TASK.md");
    fs::write(&path, valid_brief()).unwrap();
    assert_eq!(check_brief(&path), GateVerdict::Pass);
}

#[test]
fn check_brief_fails_when_one_required_clause_is_removed() {
    let dir = scratch_dir();
    let good = dir.join("good.md");
    fs::write(&good, valid_brief()).unwrap();
    assert_eq!(check_brief(&good), GateVerdict::Pass);

    let broken = valid_brief()
        .replace("不许改判据", "")
        .replace("不得修改判据", "")
        .replace("判据由", "")
        .replace("不许动判据", "");
    let path = dir.join("TASK.md");
    fs::write(&path, broken).unwrap();
    assert_eq!(check_brief(&path), GateVerdict::Fail);
}

#[test]
fn check_brief_unreadable_is_unjudgeable_not_fail() {
    let dir = scratch_dir();
    let v = check_brief(&dir.join("missing.md"));
    assert_eq!(v, GateVerdict::Unjudgeable);
    assert_ne!(v, GateVerdict::Fail);
    assert_ne!(v, GateVerdict::Pass);
}

#[test]
fn check_brief_too_short_is_fail() {
    let dir = scratch_dir();
    let path = dir.join("TASK.md");
    fs::write(&path, "太短").unwrap();
    assert_eq!(check_brief(&path), GateVerdict::Fail);
}

#[test]
fn four_state_probe_red_is_not_collapsed_to_boolean() {
    let dir = scratch_dir();
    let green = write_sh(&dir, "g.sh", "exit 0");
    let red = write_sh(&dir, "r.sh", "exit 7");
    let unj = write_sh(&dir, "u.sh", "exit 2");
    assert_eq!(GateVerdict::from_probe_exit(0), ProbeOutcome::Green);
    assert_eq!(GateVerdict::from_probe_exit(7), ProbeOutcome::Red);
    assert_eq!(GateVerdict::from_probe_exit(2), ProbeOutcome::Unjudgeable);
    assert_eq!(assert_probe_red(&green), GateVerdict::Fail);
    assert_eq!(assert_probe_red(&red), GateVerdict::Pass);
    assert_eq!(assert_probe_red(&unj), GateVerdict::Unjudgeable);
}

/// 齿：量具失效必须产生 ToolFailure，⛔ 不得塌回 Unjudgeable。
///
/// 这条守的是 fea1e2fd 新立的不变量本身。没有它，把三处
/// `Err(f) => ToolFailure(f)` 改回 `Err(_) => Unjudgeable` 不会有任何测试报警，
/// 那三条刚封住的恒绿空壳就会原样复活。
///
/// 破坏是**自己造的**（thread-local 把某一次 run_sh 的解释器换成绝对不存在的路径），
/// 因此在健康环境下也能跑、也必须绿；⛔ 不依赖运行环境恰好坏掉。
#[test]
fn tool_failure_is_never_collapsed_into_unjudgeable() {
    let dir = scratch_dir();
    let two = write_sh(&dir, "u.sh", "exit 2");
    let red = write_sh(&dir, "r.sh", "exit 1");
    let mk = write_sh(&dir, "make_green.sh", "exit 0");

    // 阳性对照：同一个 exit 2 探针，sh 健康时是真·不可判。
    let healthy = assert_probe_red(&two);
    assert_eq!(
        healthy,
        GateVerdict::Unjudgeable,
        "阳性对照失效：健康 sh 下 exit 2 必须是 Unjudgeable"
    );

    // 齿① assert_probe_red 的那次 run_sh。
    let t1 = with_broken_sh_on(&[0], || assert_probe_red(&two));
    println!("[齿① assert_probe_red      ] {t1:?}");
    assert_ne!(
        t1,
        GateVerdict::Unjudgeable,
        "量具起不来被塌回 Unjudgeable：与探针自报 exit 2 无法区分。t1={t1:?}"
    );
    assert!(
        matches!(t1, GateVerdict::ToolFailure(ShFailure::Spawn { .. })),
        "量具起不来必须是 ToolFailure(Spawn)。t1={t1:?}"
    );

    // 齿② assert_probe_two_sided 第 1 次 run_sh（坏态探针）。
    let t2 = with_broken_sh_on(&[0], || assert_probe_two_sided(&red, &mk));
    println!("[齿② two_sided 坏态探针     ] {t2:?}");
    assert_ne!(
        t2,
        GateVerdict::Unjudgeable,
        "two_sided 第一次 run_sh 塌态。t2={t2:?}"
    );
    assert!(
        matches!(t2, GateVerdict::ToolFailure(ShFailure::Spawn { .. })),
        "t2={t2:?}"
    );

    // 齿③ 第 2 次 run_sh（阳性对照 make_green）——与「对照跑了但没成功」必须是两回事。
    let t3 = with_broken_sh_on(&[1], || assert_probe_two_sided(&red, &mk));
    println!("[齿③ two_sided make_green  ] {t3:?}");
    assert_ne!(
        t3,
        GateVerdict::Unjudgeable,
        "对照脚本起不来 ≠ 对照没做成，⛔ 不许同值。t3={t3:?}"
    );
    assert!(
        matches!(t3, GateVerdict::ToolFailure(ShFailure::Spawn { .. })),
        "t3={t3:?}"
    );

    // 齿④ 第 3 次 run_sh（修好后复跑探针）。
    let flag = dir.join("fixed");
    let probe = write_sh(
        &dir,
        "probe.sh",
        &format!(
            "if [ -f '{}' ]; then exit 0; else exit 1; fi",
            flag.display()
        ),
    );
    let mk2 = write_sh(&dir, "mk2.sh", &format!("touch '{}'", flag.display()));
    let t4 = with_broken_sh_on(&[2], || assert_probe_two_sided(&probe, &mk2));
    println!("[齿④ two_sided 复跑探针     ] {t4:?}");
    assert_ne!(t4, GateVerdict::Unjudgeable, "t4={t4:?}");
    assert!(
        matches!(t4, GateVerdict::ToolFailure(ShFailure::Spawn { .. })),
        "t4={t4:?}"
    );

    // 破坏开关必须已复位：紧接着的健康调用不得再坏。
    assert_eq!(
        assert_probe_red(&two),
        GateVerdict::Unjudgeable,
        "with_broken_sh_on 没复位，破坏外溢"
    );
}
