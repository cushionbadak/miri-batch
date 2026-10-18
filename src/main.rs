//! miri-batch: 여러 개의 단일 파일 Rust 프로그램을 Miri 로 일괄 실행하고 판정표를 만든다.
//!
//! 사용법:
//!     miri-batch [INPUT...] [--list FILE]... [-j N] [-t SEC] [-o DIR] [--fixed] [-e EXT] [--toolchain TC] [--sysroot DIR] [-F FLAG]...
//!
//! INPUT 은 디렉터리 또는 소스 파일이고 여러 개를 줄 수 있다.
//!   - 디렉터리: 하위 디렉터리까지 뒤져 확장자가 맞는 파일(기본 `.rs`)을 전부 모은다. 경로순 정렬.
//!   - 소스 파일: 그 파일 하나. 확장자가 소스 확장자가 아니면 오류다(목록 파일은 `--list` 로).
//!
//! 목록 파일은 `--list FILE` 로 명시한다. 경로가 한 줄에 하나씩 적힌 텍스트 파일이고, 상대 경로는
//! 목록 파일이 있는 디렉터리 기준이며, 빈 줄과 `#` 으로 시작하는 줄은 무시한다. 여러 번 줄 수 있다.
//!
//! 각 소스 파일은 `main` 이 있는 단일 파일 프로그램이어야 한다. `--fixed` 를 주면 `.fixed` 파일
//! (rustc 테스트 스위트의 rustfix 결과물)도 소스로 취급한다.
//!
//! 동작 순서:
//!   1. 툴체인을 정하고(`--toolchain` > `MIRI_TOOLCHAIN` > 현재 활성 툴체인) Miri 드라이버 경로를
//!      `rustup which miri` 로 얻는다.
//!   2. `cargo miri setup --print-sysroot` 로 Miri 전용 sysroot 경로를 얻는다(없으면 이 명령이 만든다).
//!   3. `fn main() {}` 를 한 번 돌려 옵션이 올바른지 확인한다(preflight).
//!   4. 파일마다 Miri 드라이버를 직접 호출한다. cargo 프로젝트는 만들지 않는다.
//!      `<miri> --sysroot <sysroot> <공통 옵션> [-F 로 준 추가 옵션] --crate-name <이름> <file>`
//!      crate 이름은 파일 이름에서 만든다. rustc 가 파일 이름으로 crate 이름을 정하면 `a.b.fixed`
//!      처럼 `.` 이나 공백이 든 이름에서 실패하므로 항상 명시한다.
//!   5. 종료 코드와 stderr 로 판정(verdict)을 정하고 로그와 TSV 를 남긴다.
//!
//! 공통 옵션(설정은 이 하나뿐이다):
//!     --edition=2024 -Znext-solver=globally -Zpolonius=next -Zmir-opt-level=0
//!   앞의 셋은 2026-10 nightly 의 기본값이지만(edition 만은 bare rustc 기본이 2015 라 필수) 툴체인이
//!   바뀌어도 같은 조건이 되도록 명시한다. `-Zmir-opt-level=0` 은 Miri 의 기본값으로, MIR 최적화를
//!   완전히 끈 상태에서 검사한다(최적화는 UB 를 숨길 수 있다).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 모든 파일에 공통으로 붙는 옵션. rustc 옵션과 Miri 옵션을 구분 없이 같은 목록에 쓴다.
const COMMON_FLAGS: &[&str] = &[
    "--edition=2024",
    "-Znext-solver=globally",
    "-Zpolonius=next",
    "-Zmir-opt-level=0",
];

const USAGE: &str = "\
사용법: miri-batch [INPUT...] [--list FILE]... [옵션]

  INPUT               디렉터리(하위까지 .rs 전부) 또는 소스 파일 하나. 여러 개 가능

옵션:
  -l, --list FILE     경로 목록 파일(한 줄에 하나, 빈 줄과 # 주석 무시, 상대 경로는 FILE 기준).
                      여러 번 쓸 수 있다
  -j, --jobs N        동시에 실행할 파일 수 (기본 1 = 순차)
  -t, --timeout SEC   파일당 제한 시간(초) (기본 300)
  -o, --outdir DIR    결과 디렉터리 (기본 results/<YYYYMMDD-HHMMSS>)
      --fixed         .fixed 파일도 소스로 취급한다 (= -e fixed)
  -e, --ext EXT       소스로 취급할 확장자 추가. 여러 번 쓸 수 있다 (기본 rs)
      --toolchain TC  rustup 툴체인 이름 (기본: MIRI_TOOLCHAIN 환경 변수, 없으면 현재 활성 툴체인)
      --sysroot DIR   Miri sysroot 경로 (기본: cargo miri setup --print-sysroot 의 출력)
  -F, --flag FLAG     Miri 드라이버에 추가로 넘길 옵션. 여러 번 쓸 수 있다 (예: -F -Zmiri-tree-borrows)
  -h, --help          이 도움말
";

// ---------------------------------------------------------------------------
// 명령줄
// ---------------------------------------------------------------------------

struct Opts {
    inputs: Vec<PathBuf>,
    lists: Vec<PathBuf>,
    exts: Vec<String>,
    jobs: usize,
    timeout: Duration,
    outdir: Option<PathBuf>,
    toolchain: Option<String>,
    sysroot: Option<PathBuf>,
    extra_flags: Vec<String>,
}

fn parse_args() -> Result<Opts, String> {
    let mut args = env::args().skip(1);
    let mut inputs = Vec::new();
    let mut lists = Vec::new();
    let mut exts = vec!["rs".to_string()];
    let mut jobs = 1usize;
    let mut timeout = Duration::from_secs(300);
    let mut outdir = None;
    let mut toolchain = None;
    let mut sysroot = None;
    let mut extra_flags = Vec::new();

    fn value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("{name} 에는 값이 필요합니다"))
    }

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-j" | "--jobs" => {
                jobs = value(&mut args, "-j")?
                    .parse()
                    .map_err(|_| "-j 는 양의 정수여야 합니다")?;
                if jobs == 0 {
                    return Err("-j 는 1 이상이어야 합니다".into());
                }
            }
            "-t" | "--timeout" => {
                let secs: f64 = value(&mut args, "-t")?
                    .parse()
                    .map_err(|_| "-t 는 숫자여야 합니다")?;
                timeout = Duration::from_secs_f64(secs);
            }
            "-o" | "--outdir" => outdir = Some(PathBuf::from(value(&mut args, "-o")?)),
            "--fixed" => exts.push("fixed".to_string()),
            "-e" | "--ext" => {
                exts.push(value(&mut args, "-e")?.trim_start_matches('.').to_string())
            }
            "--toolchain" => toolchain = Some(value(&mut args, "--toolchain")?),
            "--sysroot" => sysroot = Some(PathBuf::from(value(&mut args, "--sysroot")?)),
            "-F" | "--flag" => extra_flags.push(value(&mut args, "-F")?),
            "-l" | "--list" => lists.push(PathBuf::from(value(&mut args, "--list")?)),
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("알 수 없는 옵션: {s}\n\n{USAGE}"))
            }
            _ => inputs.push(PathBuf::from(a)),
        }
    }
    if inputs.is_empty() && lists.is_empty() {
        return Err(format!(
            "입력(디렉터리나 소스 파일, 또는 --list 목록 파일)이 필요합니다\n\n{USAGE}"
        ));
    }
    exts.sort();
    exts.dedup();
    Ok(Opts {
        inputs,
        lists,
        exts,
        jobs,
        timeout,
        outdir,
        toolchain,
        sysroot,
        extra_flags,
    })
}

// ---------------------------------------------------------------------------
// 시각. 외부 crate 없이 ISO 8601(지역 시간대) 문자열을 만든다.
// 시간대 오프셋은 시작할 때 `date +%z` 로 한 번 읽는다(실패하면 UTC).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Clock {
    offset_secs: i64,
}

impl Clock {
    fn detect() -> Clock {
        let offset_secs = Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| parse_utc_offset(s.trim()))
            .unwrap_or(0);
        Clock { offset_secs }
    }

    /// 예: 2026-10-09T16:50:52.810+09:00
    fn now_iso(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let millis = now.subsec_millis();
        let local = now.as_secs() as i64 + self.offset_secs;
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let (sign, off) = if self.offset_secs < 0 {
            ('-', -self.offset_secs)
        } else {
            ('+', self.offset_secs)
        };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}{sign}{:02}:{:02}",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60,
            off / 3600,
            off % 3600 / 60
        )
    }

    /// 결과 디렉터리 이름용: 20261009-165052
    fn now_compact(&self) -> String {
        let iso = self.now_iso();
        format!(
            "{}{}{}-{}{}{}",
            &iso[0..4],
            &iso[5..7],
            &iso[8..10],
            &iso[11..13],
            &iso[14..16],
            &iso[17..19]
        )
    }
}

/// "+0900" → 32400, "-0530" → -19800
fn parse_utc_offset(s: &str) -> Option<i64> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if rest.len() != 4 {
        return None;
    }
    let h: i64 = rest[0..2].parse().ok()?;
    let m: i64 = rest[2..4].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// 1970-01-01 기준 일수 → (년, 월, 일). Howard Hinnant 의 civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// 환경: 툴체인, Miri 드라이버, sysroot
// ---------------------------------------------------------------------------

fn run_capture(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{:?} 실행 실패: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!(
            "{:?} 가 실패했습니다 ({})\n{}",
            cmd.get_program(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn resolve_toolchain(opt: &Option<String>) -> Result<String, String> {
    if let Some(t) = opt {
        return Ok(t.clone());
    }
    if let Ok(t) = env::var("MIRI_TOOLCHAIN") {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let out = run_capture(Command::new("rustup").args(["show", "active-toolchain"]))?;
    out.split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "활성 툴체인을 알 수 없습니다. --toolchain 으로 지정하세요".into())
}

fn miri_binary(toolchain: &str) -> Result<PathBuf, String> {
    let out = run_capture(Command::new("rustup").args(["which", "--toolchain", toolchain, "miri"])).map_err(|e| {
        format!("{e}\n툴체인 '{toolchain}' 에 miri 가 없습니다. 설치: rustup component add miri --toolchain {toolchain}")
    })?;
    Ok(PathBuf::from(out.trim()))
}

fn find_sysroot(toolchain: &str) -> Result<PathBuf, String> {
    let out = run_capture(Command::new("cargo").arg(format!("+{toolchain}")).args([
        "miri",
        "setup",
        "--print-sysroot",
    ]))
    .map_err(|e| format!("Miri sysroot 를 얻지 못했습니다.\n{e}"))?;
    out.lines()
        .last()
        .map(|s| PathBuf::from(s.trim()))
        .ok_or_else(|| "cargo miri setup --print-sysroot 가 아무것도 출력하지 않았습니다".into())
}

// ---------------------------------------------------------------------------
// 입력 수집: 디렉터리 / 소스 파일 / 목록 파일
// ---------------------------------------------------------------------------

fn has_source_ext(path: &Path, exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x == e))
        .unwrap_or(false)
}

/// 디렉터리를 하위까지 뒤져 확장자가 맞는 파일을 경로순으로 모은다.
fn walk_dir(dir: &Path, exts: &[String], out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{} 를 읽을 수 없습니다: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk_dir(&p, exts, out)?;
        } else if p.is_file() && has_source_ext(&p, exts) {
            out.push(p);
        }
    }
    Ok(())
}

fn parse_list(text: &str, base: &Path) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let p = Path::new(l);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            }
        })
        .collect()
}

/// 입력(디렉터리·소스 파일)과 `--list` 목록 파일을 소스 파일 목록으로 바꾼다. 절대 경로로 만들고
/// (드라이버는 결과 디렉터리에서 실행된다) 같은 파일은 한 번만 넣는다. 없는 파일이 있으면 실행 전에
/// 전부 알려 준다. 순서는 INPUT 들을 먼저, 그다음 --list 들이다.
fn collect_inputs(
    inputs: &[PathBuf],
    lists: &[PathBuf],
    exts: &[String],
) -> Result<Vec<PathBuf>, String> {
    let cwd = env::current_dir().map_err(|e| format!("현재 디렉터리를 알 수 없습니다: {e}"))?;
    let mut raw: Vec<PathBuf> = Vec::new();
    for input in inputs {
        let input = if input.is_absolute() {
            input.clone()
        } else {
            cwd.join(input)
        };
        if input.is_dir() {
            let before = raw.len();
            walk_dir(&input, exts, &mut raw)?;
            if raw.len() == before {
                eprintln!(
                    "경고: {} 안에 .{} 파일이 없습니다",
                    input.display(),
                    exts.join("/.")
                );
            }
        } else if input.is_file() && has_source_ext(&input, exts) {
            raw.push(input);
        } else if input.is_file() {
            return Err(format!(
                "{} 는 소스 파일(.{})이 아닙니다. 경로 목록 파일이라면 --list {} 로 주세요",
                input.display(),
                exts.join("/."),
                input.display()
            ));
        } else {
            return Err(format!("입력이 없습니다: {}", input.display()));
        }
    }
    for list in lists {
        let list = if list.is_absolute() {
            list.clone()
        } else {
            cwd.join(list)
        };
        let text = fs::read_to_string(&list)
            .map_err(|e| format!("목록 파일 {} 를 읽을 수 없습니다: {e}", list.display()))?;
        let base = list.parent().unwrap_or(Path::new("."));
        let before = raw.len();
        raw.extend(parse_list(&text, base));
        if raw.len() == before {
            eprintln!("경고: 목록 파일 {} 에 경로가 없습니다", list.display());
        }
    }

    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for p in raw {
        let abs = if p.is_absolute() { p } else { cwd.join(p) };
        if !abs.is_file() {
            missing.push(abs);
            continue;
        }
        let canon = fs::canonicalize(&abs).unwrap_or(abs);
        if seen.insert(canon.clone()) {
            files.push(canon);
        }
    }
    if !missing.is_empty() {
        let list = missing
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("없는 파일:\n{list}"));
    }
    if files.is_empty() {
        return Err("실행할 파일이 없습니다".into());
    }
    Ok(files)
}

/// 파일 이름 → rustc 가 받아들이는 crate 이름. 영숫자와 `_` 외의 문자는 `_` 로 바꾼다.
fn crate_name_for(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let mut name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name.push_str("file");
    }
    if name.as_bytes()[0].is_ascii_digit() {
        name.insert(0, '_');
    }
    name
}

// ---------------------------------------------------------------------------
// 판정
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Timeout,
    Ice,
    ConfigError,
    Ub,
    Leak,
    Unsupported,
    CompileError,
    Panic,
    Ok,
    Exit(i32),
    Signal,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Timeout => write!(f, "TIMEOUT"),
            Verdict::Ice => write!(f, "ICE"),
            Verdict::ConfigError => write!(f, "CONFIG_ERROR"),
            Verdict::Ub => write!(f, "UB"),
            Verdict::Leak => write!(f, "LEAK"),
            Verdict::Unsupported => write!(f, "UNSUPPORTED"),
            Verdict::CompileError => write!(f, "COMPILE_ERROR"),
            Verdict::Panic => write!(f, "PANIC"),
            Verdict::Ok => write!(f, "OK"),
            Verdict::Exit(c) => write!(f, "EXIT_{c}"),
            Verdict::Signal => write!(f, "SIGNAL"),
        }
    }
}

/// 종료 코드만으로는 UB·컴파일 오류·누수가 모두 1, ICE·패닉이 모두 101 이라 구분이 안 된다.
/// 그래서 stderr 문자열을 1차 기준으로 쓴다. 위에서부터 먼저 맞는 것이 판정이다.
fn classify(exit_code: Option<i32>, stderr: &str, timed_out: bool) -> Verdict {
    if timed_out {
        return Verdict::Timeout;
    }
    if stderr.contains("the compiler unexpectedly panicked") || stderr.contains("thread 'rustc'") {
        return Verdict::Ice;
    }
    if stderr.contains("unknown unstable option")
        || stderr.contains("unknown codegen option")
        || stderr.contains("given more than once")
        || stderr.contains("Unrecognized option")
    {
        return Verdict::ConfigError;
    }
    if stderr.contains("error: Undefined Behavior") {
        return Verdict::Ub;
    }
    if stderr.contains("error: memory leaked") {
        return Verdict::Leak;
    }
    if stderr.contains("error: unsupported operation") {
        return Verdict::Unsupported;
    }
    if stderr.lines().any(|l| l.starts_with("error[E")) || stderr.contains("error: aborting due to")
    {
        return Verdict::CompileError;
    }
    match exit_code {
        Some(101) if stderr.contains("panicked at") => Verdict::Panic,
        Some(0) => Verdict::Ok,
        Some(c) => Verdict::Exit(c),
        None => Verdict::Signal,
    }
}

fn first_error_line(stderr: &str, stdout: &str) -> String {
    stderr
        .lines()
        .find(|l| l.starts_with("error") || l.contains("panicked at"))
        .or_else(|| stdout.lines().next())
        .unwrap_or("")
        .trim()
        .to_owned()
}

// ---------------------------------------------------------------------------
// 파일 하나 실행
// ---------------------------------------------------------------------------

struct OutLine {
    at: Duration,         // 작업 시작 기준 경과
    stream: &'static str, // "out" | "err"
    text: String,
}

struct Outcome {
    verdict: Verdict,
    exit_code: Option<i32>,
    timed_out: bool,
    started: String,
    ended: String,
    elapsed: Duration,
    lines: Vec<OutLine>,
    first_error: String,
}

struct Driver {
    bin: PathBuf,
    sysroot: PathBuf,
    flags: Vec<String>,
    timeout: Duration,
    ice_dir: PathBuf,
    clock: Clock,
}

impl Driver {
    fn command(&self, file: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.arg("--sysroot")
            .arg(&self.sysroot)
            .args(&self.flags)
            .arg("--crate-name")
            .arg(crate_name_for(file))
            .arg(file);
        c
    }

    fn command_line(&self, file: &Path) -> String {
        let c = self.command(file);
        let mut s = c.get_program().to_string_lossy().into_owned();
        for a in c.get_args() {
            s.push(' ');
            s.push_str(&a.to_string_lossy());
        }
        s
    }

    fn run(&self, file: &Path) -> io::Result<Outcome> {
        let started = self.clock.now_iso();
        let t0 = Instant::now();
        let mut child: Child = self
            .command(file)
            .env("RUSTC_ICE", &self.ice_dir) // 컴파일러가 죽으면 덤프를 여기에
            .current_dir(&self.ice_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let lines = Arc::new(Mutex::new(Vec::<OutLine>::new()));
        let mut readers = Vec::new();
        for (stream, reader) in [
            (
                "out",
                Box::new(child.stdout.take().unwrap()) as Box<dyn io::Read + Send>,
            ),
            ("err", Box::new(child.stderr.take().unwrap())),
        ] {
            let lines = Arc::clone(&lines);
            readers.push(thread::spawn(move || {
                for line in BufReader::new(reader).split(b'\n') {
                    let Ok(bytes) = line else { break };
                    let text = String::from_utf8_lossy(&bytes)
                        .trim_end_matches('\r')
                        .to_owned();
                    lines.lock().unwrap().push(OutLine {
                        at: t0.elapsed(),
                        stream,
                        text,
                    });
                }
            }));
        }

        // 제한 시간: 주기적으로 try_wait 하고 넘으면 kill. Miri 드라이버는 자식 프로세스를 만들지
        // 않으므로(확인함) 드라이버 하나만 죽이면 된다.
        let mut timed_out = false;
        let status: Option<ExitStatus> = loop {
            if let Some(st) = child.try_wait()? {
                break Some(st);
            }
            if t0.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            thread::sleep(Duration::from_millis(5));
        };
        for r in readers {
            let _ = r.join();
        }
        let elapsed = t0.elapsed();
        let ended = self.clock.now_iso();

        let lines = Arc::try_unwrap(lines)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default();
        let exit_code = status.and_then(|s| s.code());
        let stderr: String = lines
            .iter()
            .filter(|l| l.stream == "err")
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let stdout: String = lines
            .iter()
            .filter(|l| l.stream == "out")
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let verdict = classify(exit_code, &stderr, timed_out);
        let first_error = first_error_line(&stderr, &stdout);
        Ok(Outcome {
            verdict,
            exit_code,
            timed_out,
            started,
            ended,
            elapsed,
            lines,
            first_error,
        })
    }
}

fn write_job_log(
    path: &Path,
    file: &Path,
    cmdline: &str,
    toolchain: &str,
    sysroot: &Path,
    o: &Outcome,
) -> io::Result<()> {
    let mut f = io::BufWriter::new(File::create(path)?);
    writeln!(f, "# file     : {}", file.display())?;
    writeln!(f, "# command  : {cmdline}")?;
    writeln!(f, "# toolchain: {toolchain}")?;
    writeln!(f, "# sysroot  : {}", sysroot.display())?;
    writeln!(f, "# started  : {}", o.started)?;
    writeln!(
        f,
        "# ---- output (out=stdout err=stderr, [+초] = 작업 시작 기준 경과) ----"
    )?;
    for l in &o.lines {
        writeln!(f, "[+{:8.3}] {} | {}", l.at.as_secs_f64(), l.stream, l.text)?;
    }
    writeln!(f, "# ---- end ----")?;
    writeln!(f, "# ended    : {}", o.ended)?;
    writeln!(f, "# elapsed  : {:.3}s", o.elapsed.as_secs_f64())?;
    writeln!(f, "# exit     : {}", exit_text(o))?;
    writeln!(f, "# verdict  : {}", o.verdict)?;
    Ok(())
}

fn exit_text(o: &Outcome) -> String {
    if o.timed_out {
        "timeout".into()
    } else {
        o.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into())
    }
}

// ---------------------------------------------------------------------------
// 전체 흐름
// ---------------------------------------------------------------------------

struct RunLog {
    file: Mutex<File>,
    clock: Clock,
}

impl RunLog {
    fn write(&self, fields: &[&str]) {
        let mut f = self.file.lock().unwrap();
        let _ = writeln!(f, "{}\t{}", self.clock.now_iso(), fields.join("\t"));
        let _ = f.flush();
    }
}

struct Record {
    index: usize,
    file: PathBuf,
    outcome: Outcome,
    log_path: PathBuf,
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("오류: {e}");
        std::process::exit(2);
    }
}

fn real_main() -> Result<(), String> {
    let opts = parse_args()?;
    let clock = Clock::detect();

    let files = collect_inputs(&opts.inputs, &opts.lists, &opts.exts)?;

    let toolchain = resolve_toolchain(&opts.toolchain)?;
    let bin = miri_binary(&toolchain)?;
    let sysroot = match &opts.sysroot {
        Some(s) => s.clone(),
        None => find_sysroot(&toolchain)?,
    };
    let outdir = opts
        .outdir
        .clone()
        .unwrap_or_else(|| PathBuf::from("results").join(clock.now_compact()));
    let logs_dir = outdir.join("logs");
    let ice_dir = outdir.join("ice");
    fs::create_dir_all(&logs_dir).map_err(|e| format!("{} 생성 실패: {e}", logs_dir.display()))?;
    fs::create_dir_all(&ice_dir).map_err(|e| format!("{} 생성 실패: {e}", ice_dir.display()))?;
    // 드라이버는 ICE 덤프를 위해 자식 프로세스의 cwd 를 `ice_dir` 로 바꾼다(아래 `Driver::run`).
    // 그래서 상대 경로로 넘긴 소스 파일(특히 preflight 파일)은 `ice_dir` 기준으로 풀려 못 찾는다.
    // 결과 디렉터리를 절대 경로로 고정해 logs/ice/preflight 경로가 모두 절대 경로가 되게 한다.
    let ice_dir = fs::canonicalize(&ice_dir).unwrap_or(ice_dir);
    let outdir = ice_dir.parent().map(|p| p.to_path_buf()).unwrap_or(outdir);
    let logs_dir = outdir.join("logs");

    let mut flags: Vec<String> = COMMON_FLAGS.iter().map(|s| s.to_string()).collect();
    flags.extend(opts.extra_flags.iter().cloned());
    fs::write(outdir.join("flags.txt"), flags.join(" ") + "\n").map_err(|e| e.to_string())?;
    fs::write(
        outdir.join("files.txt"),
        files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .map_err(|e| e.to_string())?;

    let run_log = RunLog {
        file: Mutex::new(File::create(outdir.join("run.log")).map_err(|e| e.to_string())?),
        clock,
    };
    run_log.write(&[
        "BATCH_START",
        &format!("toolchain={toolchain}"),
        &format!("miri={}", bin.display()),
        &format!("sysroot={}", sysroot.display()),
        &format!("files={}", files.len()),
        &format!("exts={}", opts.exts.join(",")),
        &format!("jobs={}", opts.jobs),
        &format!("timeout={}s", opts.timeout.as_secs_f64()),
        &format!("flags={}", flags.join(" ")),
    ]);

    let driver = Driver {
        bin,
        sysroot: sysroot.clone(),
        flags,
        timeout: opts.timeout,
        ice_dir,
        clock,
    };

    // preflight: 빈 프로그램이 OK 가 아니면 옵션이나 환경 문제다. 파일 전부를 잘못 판정하기 전에 멈춘다.
    let empty = outdir.join("preflight_empty.rs");
    fs::write(&empty, "fn main() {}\n").map_err(|e| e.to_string())?;
    let pre = driver
        .run(&empty)
        .map_err(|e| format!("preflight 실행 실패: {e}"))?;
    run_log.write(&["PREFLIGHT", &pre.verdict.to_string()]);
    if pre.verdict != Verdict::Ok {
        let err: Vec<_> = pre
            .lines
            .iter()
            .filter(|l| l.stream == "err")
            .map(|l| l.text.as_str())
            .collect();
        return Err(format!(
            "preflight 실패 ({}): 빈 프로그램 `fn main() {{}}` 가 통과하지 못했습니다.\n명령: {}\n{}",
            pre.verdict,
            driver.command_line(&empty),
            err.join("\n")
        ));
    }

    println!(
        "파일 {}개 (.{}), 동시 {}, 제한 {}s, 툴체인 {}\n옵션: {}\n결과: {}/",
        files.len(),
        opts.exts.join("/."),
        opts.jobs,
        opts.timeout.as_secs_f64(),
        toolchain,
        driver.flags.join(" "),
        outdir.display()
    );

    // 작업 큐: 공유 카운터를 원자적으로 증가시켜 다음 파일을 가져간다. 끝나는 대로 다음 것을 집어 가므로
    // 긴 파일이 한 스레드에 몰려도 다른 스레드가 놀지 않는다.
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let records: Mutex<Vec<Record>> = Mutex::new(Vec::with_capacity(files.len()));
    let stdout_lock = Mutex::new(());
    let width = files.len().to_string().len();
    let t_batch = Instant::now();

    thread::scope(|s| {
        for _ in 0..opts.jobs.min(files.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let file = &files[i];
                let index = i + 1;
                run_log.write(&[
                    "START",
                    &format!("#{index:05}"),
                    &file.display().to_string(),
                ]);
                let outcome = match driver.run(file) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("#{index} {} 실행 실패: {e}", file.display());
                        continue;
                    }
                };
                let name = file
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let log_path = logs_dir.join(format!("{index:04}_{name}.log"));
                if let Err(e) = write_job_log(
                    &log_path,
                    file,
                    &driver.command_line(file),
                    &toolchain,
                    &sysroot,
                    &outcome,
                ) {
                    eprintln!("로그 쓰기 실패 {}: {e}", log_path.display());
                }
                run_log.write(&[
                    "END",
                    &format!("#{index:05}"),
                    &outcome.verdict.to_string(),
                    &format!("exit={}", exit_text(&outcome)),
                    &format!("{:.3}s", outcome.elapsed.as_secs_f64()),
                    &file.display().to_string(),
                ]);
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                {
                    let _g = stdout_lock.lock().unwrap();
                    println!(
                        "[{n:>width$}/{}] {:<13} {:7.2}s  {}",
                        files.len(),
                        outcome.verdict.to_string(),
                        outcome.elapsed.as_secs_f64(),
                        name
                    );
                }
                records.lock().unwrap().push(Record {
                    index,
                    file: file.clone(),
                    outcome,
                    log_path,
                });
            });
        }
    });
    let total = t_batch.elapsed();

    let mut records = records.into_inner().unwrap();
    records.sort_by_key(|r| r.index);

    // results.tsv
    let mut tsv =
        io::BufWriter::new(File::create(outdir.join("results.tsv")).map_err(|e| e.to_string())?);
    writeln!(
        tsv,
        "index\tfile\tverdict\texit\tstarted\telapsed_s\tfirst_error\tlog"
    )
    .map_err(|e| e.to_string())?;
    for r in &records {
        writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{}",
            r.index,
            r.file.display(),
            r.outcome.verdict,
            exit_text(&r.outcome),
            r.outcome.started,
            r.outcome.elapsed.as_secs_f64(),
            r.outcome.first_error.replace('\t', " "),
            r.log_path.display()
        )
        .map_err(|e| e.to_string())?;
    }
    drop(tsv);

    // 화면 요약
    let mut counts: BTreeMap<Verdict, usize> = BTreeMap::new();
    for r in &records {
        *counts.entry(r.outcome.verdict).or_default() += 1;
    }
    println!();
    println!(
        "총 {}개 파일, {:.1}s (벽시계). 결과: {}/",
        records.len(),
        total.as_secs_f64(),
        outdir.display()
    );
    for (v, n) in &counts {
        println!("  {:<13} {n:>6}", v.to_string());
    }
    let interesting: Vec<_> = records
        .iter()
        .filter(|r| !matches!(r.outcome.verdict, Verdict::Ok | Verdict::CompileError))
        .collect();
    if !interesting.is_empty() {
        println!("OK/COMPILE_ERROR 가 아닌 파일:");
        for r in interesting {
            println!(
                "  {:<13} {}",
                r.outcome.verdict.to_string(),
                r.file.display()
            );
        }
    }
    run_log.write(&["BATCH_END", &format!("total={:.3}s", total.as_secs_f64())]);
    Ok(())
}

// ---------------------------------------------------------------------------
// 테스트: cargo test
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_stderr_text() {
        assert_eq!(
            classify(
                Some(1),
                "error: Undefined Behavior: constructing invalid value",
                false
            ),
            Verdict::Ub
        );
        assert_eq!(
            classify(Some(1), "error: memory leaked: alloc1", false),
            Verdict::Leak
        );
        assert_eq!(
            classify(
                Some(1),
                "error: unsupported operation: can't call foreign function",
                false
            ),
            Verdict::Unsupported
        );
        assert_eq!(
            classify(
                Some(1),
                "error[E0308]: mismatched types\nerror: aborting due to 1 previous error",
                false
            ),
            Verdict::CompileError
        );
        assert_eq!(
            classify(Some(1), "error: unknown unstable option: `no-such`", false),
            Verdict::ConfigError
        );
        assert_eq!(
            classify(
                Some(101),
                "thread 'rustc' (1) panicked at compiler/x.rs:1:1:\nerror: the compiler unexpectedly panicked. This is a bug",
                false
            ),
            Verdict::Ice
        );
        assert_eq!(
            classify(
                Some(101),
                "thread 'main' (1000) panicked at a.rs:1:12:\nboom",
                false
            ),
            Verdict::Panic
        );
        assert_eq!(classify(Some(0), "", false), Verdict::Ok);
        assert_eq!(classify(Some(7), "", false), Verdict::Exit(7));
        assert_eq!(classify(None, "", false), Verdict::Signal);
        assert_eq!(classify(None, "anything", true), Verdict::Timeout);
    }

    #[test]
    fn timeout_beats_everything() {
        assert_eq!(
            classify(Some(1), "error: Undefined Behavior", true),
            Verdict::Timeout
        );
    }

    #[test]
    fn list_parsing_ignores_comments_and_resolves_relative_paths() {
        let base = Path::new("/base");
        let v = parse_list("# c\n\n/abs/a.rs\n  rel/b.rs  \n", base);
        assert_eq!(
            v,
            vec![PathBuf::from("/abs/a.rs"), PathBuf::from("/base/rel/b.rs")]
        );
    }

    #[test]
    fn crate_names_are_sanitized() {
        assert_eq!(crate_name_for(Path::new("/x/plain.rs")), "plain");
        assert_eq!(
            crate_name_for(Path::new("/x/issue-12345.fixed")),
            "issue_12345"
        );
        assert_eq!(crate_name_for(Path::new("/x/test.rev1.fixed")), "test_rev1");
        assert_eq!(crate_name_for(Path::new("/x/with space.rs")), "with_space");
        assert_eq!(crate_name_for(Path::new("/x/1abc.rs")), "_1abc");
        assert_eq!(crate_name_for(Path::new("/x/.rs")), "_rs"); // 점으로 시작하면 stem 이 ".rs"
    }

    #[test]
    fn directory_walk_is_recursive_sorted_and_filtered_by_extension() {
        let root = env::temp_dir().join(format!("miri-batch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("b/nested")).unwrap();
        fs::create_dir_all(root.join("a")).unwrap();
        for rel in ["a/z.rs", "a/y.fixed", "b/nested/x.rs", "b/w.txt", "top.rs"] {
            fs::write(root.join(rel), "fn main() {}\n").unwrap();
        }
        let rs = vec!["rs".to_string()];
        let mut out = Vec::new();
        walk_dir(&root, &rs, &mut out).unwrap();
        let rel: Vec<_> = out
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(rel, vec!["a/z.rs", "b/nested/x.rs", "top.rs"]);

        let both = vec!["fixed".to_string(), "rs".to_string()];
        let mut out = Vec::new();
        walk_dir(&root, &both, &mut out).unwrap();
        assert_eq!(out.len(), 4);
        assert!(out.iter().any(|p| p.ends_with("a/y.fixed")));

        // collect_inputs: 디렉터리 + 같은 파일을 가리키는 --list 목록 → 중복 제거
        let list = root.join("list.txt");
        fs::write(&list, "top.rs\na/z.rs\n").unwrap();
        let files = collect_inputs(&[root.clone()], &[list.clone()], &rs).unwrap();
        assert_eq!(files.len(), 3);
        // --list 만 주면 목록의 순서대로
        let files = collect_inputs(&[], &[list.clone()], &rs).unwrap();
        assert!(files[0].ends_with("top.rs") && files[1].ends_with("a/z.rs"));
        // 소스 확장자가 아닌 파일을 INPUT 으로 주면 오류(목록 파일로 오해하지 않는다)
        let err = collect_inputs(&[list.clone()], &[], &rs).unwrap_err();
        assert!(err.contains("--list"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn utc_offset_parsing() {
        assert_eq!(parse_utc_offset("+0900"), Some(32400));
        assert_eq!(parse_utc_offset("-0530"), Some(-19800));
        assert_eq!(parse_utc_offset("0900"), None);
        assert_eq!(parse_utc_offset("+09"), None);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(20_735), (2026, 10, 9)); // 2026-10-09
        assert_eq!(civil_from_days(11_016), (2000, 2, 29)); // 윤년
    }

    #[test]
    fn iso_format_shape() {
        let c = Clock { offset_secs: 32400 };
        let s = c.now_iso();
        assert_eq!(s.len(), "2026-10-09T16:50:52.810+09:00".len(), "{s}");
        assert!(s.ends_with("+09:00"));
        assert_eq!(c.now_compact().len(), "20261009-165052".len());
    }

    #[test]
    fn verdict_display() {
        assert_eq!(Verdict::Exit(7).to_string(), "EXIT_7");
        assert_eq!(Verdict::CompileError.to_string(), "COMPILE_ERROR");
    }
}
