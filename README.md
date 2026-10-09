# miri-batch

**여러 개의 단일 파일 Rust 프로그램을 Miri로 한꺼번에 돌리고, 파일마다 "UB / OK / 컴파일 오류 / 패닉 / 타임아웃" 판정을 표로 만들어 주는 도구다.** 외부 crate 없이 Rust 표준 라이브러리만으로 작성되어 stable 툴체인에서 `cargo build`로 빌드된다. 실행 설정은 하나뿐이며 파일당 0.05초 안팎이 걸린다.

처음 쓰는 사람은 [TUTORIAL.md](TUTORIAL.md)를 따라 하면 된다. 왜 이렇게 만들었는지는 [DESIGN.md](DESIGN.md)에 있다.

## 빠른 시작

```bash
# 1. Miri 준비 (nightly 툴체인에 miri 컴포넌트 추가, 최초 한 번)
rustup toolchain install nightly
rustup component add miri --toolchain nightly

# 2. 빌드 (stable 로 충분)
cargo build --release

# 3. 샘플 9개 실행 (디렉터리를 그대로 입력)
./target/release/miri-batch samples

# 4. 내 디렉터리 전체 (하위 폴더 포함, 8개 동시, 파일당 60초 제한)
./target/release/miri-batch /path/to/cases -j 8 -t 60

# 5. .fixed 파일도 함께
./target/release/miri-batch /path/to/ui-tests --fixed
```

입력은 세 종류이고 여러 개를 섞어 줄 수 있다.

| 입력 | 동작 |
| --- | --- |
| 디렉터리 | 하위 디렉터리까지 뒤져 `.rs` 파일을 전부 모은다. 경로순 정렬. `--fixed`를 주면 `.fixed`도, `-e EXT`로 다른 확장자도 추가 |
| 소스 파일 (`.rs` 등) | 그 파일 하나 |
| 목록 파일 (그 외) | 경로가 한 줄에 하나씩 있는 텍스트. 상대 경로는 목록 파일이 있는 디렉터리 기준. 빈 줄과 `#` 주석 무시 |

같은 파일이 여러 입력에서 겹치면 한 번만 돌린다. 각 파일은 `main`이 있는 단일 파일 프로그램이어야 한다. 실제로 돌린 파일 목록은 결과 디렉터리의 `files.txt`에 남는다.

## 실행 설정은 하나다

모든 파일을 아래 옵션으로 돌린다. 설정을 여러 개 두지 않는다.

```
--edition=2024 -Znext-solver=globally -Zpolonius=next -Zmir-opt-level=0
```

| 옵션 | 뜻 | 비고 |
| --- | --- | --- |
| `--edition=2024` | Rust 2024 edition | bare rustc의 기본은 2015라 반드시 명시 |
| `-Znext-solver=globally` | 새 trait solver를 전역으로 사용 | 2026-10 nightly의 기본값. 고정을 위해 명시 |
| `-Zpolonius=next` | 새 borrow checker(polonius) 사용 | 2026-10 nightly의 기본값. 고정을 위해 명시 |
| `-Zmir-opt-level=0` | MIR 최적화 완전히 끔 | Miri의 기본값. 최적화는 UB를 숨길 수 있어 끈 상태로 검사 |

옵션을 더 붙이고 싶으면 `-F`를 쓴다. 예: `-F -Zmiri-tree-borrows`. 시작할 때 `fn main() {}`로 사전 점검을 하므로 오타 난 옵션은 파일을 돌리기 전에 걸린다.

## 결과물

실행마다 `results/<YYYYMMDD-HHMMSS>/`가 생긴다(`-o`로 바꿀 수 있다).

| 파일 | 내용 |
| --- | --- |
| `results.tsv` | 파일마다 한 줄: 순번, 경로, 판정, 종료 코드, 시작 시각, 소요 초, 오류 첫 줄, 로그 경로 |
| `logs/NNNN_<파일명>.log` | 그 파일의 전체 출력. 머리말(명령줄, 시작 시각), 본문(줄마다 `[+경과초] out/err`), 꼬리말(종료 시각, 종료 코드, 판정) |
| `run.log` | 시각순 사건 기록: 배치 시작, 사전 점검, 각 파일의 START/END, 배치 종료 |
| `flags.txt` | 실제로 쓴 옵션 |
| `files.txt` | 실제로 돌린 파일의 절대 경로 목록(입력 순서) |
| `ice/` | 컴파일러 자체가 죽었을 때(ICE) rustc가 남기는 덤프 |

화면에는 진행 상황과 판정별 개수, 그리고 OK도 컴파일 오류도 아닌 파일 목록이 나온다.

## 판정(verdict)

종료 코드만으로는 구분이 안 된다. UB, 컴파일 오류, 메모리 누수가 모두 1이고 ICE와 패닉이 모두 101이다. 그래서 stderr 문자열을 1차 기준으로 쓴다. 위에서부터 먼저 맞는 것이 판정이다.

| 판정 | 조건 | 뜻 |
| --- | --- | --- |
| `TIMEOUT` | 제한 시간 초과 | 프로그램을 강제 종료함 |
| `ICE` | `the compiler unexpectedly panicked` | 컴파일러 자체의 버그. `ice/`에 덤프 |
| `CONFIG_ERROR` | `unknown unstable option` 등 | 옵션 오타. 보통 사전 점검에서 걸림 |
| `UB` | `error: Undefined Behavior` | Miri가 정의되지 않은 동작을 발견 |
| `LEAK` | `error: memory leaked` | 종료 시 해제되지 않은 메모리 |
| `UNSUPPORTED` | `error: unsupported operation` | Miri가 지원하지 않는 API(네트워크 등) |
| `COMPILE_ERROR` | `error[E....]` | 컴파일 실패 |
| `PANIC` | 종료 101 + `panicked at` | 프로그램이 패닉 |
| `OK` | 종료 0 | 정상 종료 |
| `EXIT_n` | 그 외 종료 코드 n | 프로그램이 스스로 n으로 종료 |

## 옵션

```
miri-batch INPUT... [옵션]
  INPUT               디렉터리, 소스 파일, 또는 경로 목록 파일. 여러 개 가능
  -j, --jobs N        동시에 실행할 파일 수 (기본 1 = 순차)
  -t, --timeout SEC   파일당 제한 시간(초) (기본 300)
  -o, --outdir DIR    결과 디렉터리 (기본 results/<YYYYMMDD-HHMMSS>)
      --fixed         .fixed 파일도 소스로 취급 (= -e fixed)
  -e, --ext EXT       소스로 취급할 확장자 추가. 여러 번 가능 (기본 rs)
      --toolchain TC  rustup 툴체인 이름 (기본: MIRI_TOOLCHAIN 환경 변수, 없으면 현재 활성 툴체인)
      --sysroot DIR   Miri sysroot 경로 (기본: cargo miri setup --print-sysroot 의 출력)
  -F, --flag FLAG     Miri 드라이버에 추가로 넘길 옵션. 여러 번 쓸 수 있다
  -h, --help          도움말
```

## 요구 사항

- rustup, 그리고 miri 컴포넌트가 설치된 nightly 툴체인(실행 대상). 검증한 버전: `nightly-2026-10-01`, miri 0.1.0.
- 도구 자체를 빌드할 툴체인은 stable 1.79 이상이면 된다(검증: 1.95.0).
- Linux에서 검증했다. 시간대는 `date +%z`로 읽으므로 그 명령이 없으면 UTC로 찍힌다.
