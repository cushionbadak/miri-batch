# miri-batch 튜토리얼

**이 문서를 끝까지 따라 하면, `.rs` 파일 수백 개를 Miri로 자동 검사해 어떤 파일에 정의되지 않은 동작(UB)이 있는지 표로 받아 볼 수 있다.** Rust 문법과 cargo만 알면 된다. Miri를 써 본 적이 없어도 괜찮다. 검증 환경은 Linux, rustup 1.29, nightly-2026-10-01이다.

## 0. Miri가 무엇인지 1분 요약

Miri는 Rust 공식 UB 검출기다. 프로그램을 기계어로 컴파일하지 않고 컴파일러 중간 표현(MIR)을 한 줄씩 해석하면서, 해제된 메모리 접근, 초기화되지 않은 값 읽기, 참조 별칭 규칙 위반 같은 것을 실행 시점에 잡아낸다. 컴파일러가 통과시킨 코드에서도 잡는다.

알아둘 것 세 가지:

- nightly 전용이다. 컴파일러 내부 API에 의존한다.
- 네이티브 실행보다 수십~수백 배 느리다. 그래서 작은 단일 파일 프로그램에 잘 맞는다.
- 실행된 경로만 검사한다. 실행되지 않은 코드의 UB는 못 잡는다.

## 1. 준비: nightly와 miri 컴포넌트

```bash
rustup --version                                  # rustup 이 있어야 한다. 없으면 https://rustup.rs
rustup toolchain install nightly                  # nightly 툴체인
rustup component add miri --toolchain nightly     # 그 툴체인에 miri 추가 (몇 초)
cargo +nightly miri --version                     # 확인. 예: miri 0.1.0 (21b707e3f9 2026-09-30)
```

`+nightly`는 "이 명령을 nightly 툴체인으로 실행하라"는 뜻이다. 날짜를 고정하고 싶으면 `nightly-2026-10-01`처럼 쓴다. **miri 컴포넌트는 툴체인마다 따로 설치된다.** 어떤 툴체인에 설치했는지 기억해 두자.

처음 한 번은 Miri 전용 표준 라이브러리(sysroot)를 만든다. 자동으로도 되지만 미리 해 두면 첫 실행이 깔끔하다.

```bash
cargo +nightly miri setup
# ... A sysroot for Miri is now available in `/home/<user>/.cache/miri`.
```

약 16초 걸리고 한 번만 하면 된다.

## 2. 빌드

이 도구는 외부 crate가 없다. stable로 빌드한다.

```bash
cd miri-batch
cargo build --release
./target/release/miri-batch --help
```

`cargo test`로 판정 규칙과 날짜 계산의 단위 테스트 7개가 통과하는지 확인할 수 있다.

## 3. 샘플로 첫 실행

프로젝트에 들어 있는 샘플 9개를 돌려 보자. 파일당 3초 제한을 걸어 두면 일부러 무한 루프를 도는 샘플이 빨리 끝난다.

```bash
./target/release/miri-batch samples/list.txt -t 3
```

출력:

```
파일 9개, 동시 1, 제한 3s, 툴체인 nightly-2026-10-01-x86_64-unknown-linux-gnu
옵션: --edition=2024 -Znext-solver=globally -Zpolonius=next -Zmir-opt-level=0
결과: results/20261009-170504/
[1/9] COMPILE_ERROR    0.03s  compile_err.rs
[2/9] UB               0.04s  dangling_stack.rs
[3/9] EXIT_7           0.04s  ex7.rs
[4/9] TIMEOUT          3.01s  hang.rs
[5/9] UB               0.05s  issue_163886.rs
[6/9] OK               0.05s  ok.rs
[7/9] PANIC            0.06s  overflow.rs
[8/9] PANIC            0.06s  panic.rs
[9/9] UB               0.04s  uaf_box.rs

총 9개 파일, 3.4s (벽시계). 결과: results/20261009-170504/
  TIMEOUT            1
  UB                 3
  COMPILE_ERROR      1
  PANIC              2
  OK                 1
  EXIT_7             1
OK/COMPILE_ERROR 가 아닌 파일:
  UB            .../samples/dangling_stack.rs
  ...
```

샘플이 무엇을 보여 주는지:

| 샘플 | 판정 | 왜 |
| --- | --- | --- |
| `ok.rs` | OK | 정상 프로그램 |
| `uaf_box.rs` | UB | `Box`를 해제한 뒤 원시 포인터로 읽음 |
| `dangling_stack.rs` | UB | 블록을 벗어난 지역 변수를 포인터로 읽음 |
| `issue_163886.rs` | UB | borrow checker가 놓친 수명 버그(rust-lang/rust#163886). 컴파일은 되지만 해제된 메모리를 읽음 |
| `overflow.rs` | PANIC | `255u8 + 1`. 디버그 의미라 overflow 검사가 켜져 있어 패닉 |
| `panic.rs` | PANIC | `panic!()` |
| `ex7.rs` | EXIT_7 | `std::process::exit(7)` |
| `compile_err.rs` | COMPILE_ERROR | 타입 오류 |
| `hang.rs` | TIMEOUT | 아주 긴 루프. 제한 시간에 강제 종료 |

## 4. 결과 읽기

결과 디렉터리에서 가장 먼저 볼 것은 `results.tsv`다. 탭으로 구분되어 있어 터미널에서는 `column`으로, 아니면 스프레드시트로 연다.

```bash
column -t -s $'\t' results/20261009-170504/results.tsv
```

```
index  file                       verdict        exit     started                        elapsed_s  first_error
1      samples/compile_err.rs     COMPILE_ERROR  1        2026-10-09T17:05:04.530+09:00  0.026      error[E0308]: mismatched types
2      samples/dangling_stack.rs  UB             1        2026-10-09T17:05:04.555+09:00  0.036      error: Undefined Behavior: memory access failed: alloc206 has been freed
5      samples/issue_163886.rs    UB             1        2026-10-09T17:05:07.637+09:00  0.046      error: Undefined Behavior: constructing invalid value of type &u8: ...
```

UB로 판정된 파일은 `logs/`의 해당 로그를 연다. 줄마다 작업 시작 기준 경과 초와 스트림(out/err)이 붙어 있다.

```
# file     : samples/issue_163886.rs
# command  : /home/.../bin/miri --sysroot /home/.../.cache/miri --edition=2024 -Znext-solver=globally -Zpolonius=next -Zmir-opt-level=0 samples/issue_163886.rs
# started  : 2026-10-09T17:05:07.637+09:00
# ---- output (out=stdout err=stderr, [+초] = 작업 시작 기준 경과) ----
[+   0.035] err | error: Undefined Behavior: constructing invalid value of type &u8: encountered a dangling reference (use-after-free)
[+   0.035] err |   --> samples/issue_163886.rs:33:10
[+   0.035] err |    |
[+   0.035] err | 33 |     show(escaped, &reused);
[+   0.035] err |    |          ^^^^^^^ Undefined Behavior occurred here
...
# ended    : 2026-10-09T17:05:07.682+09:00
# elapsed  : 0.046s
# exit     : 1
# verdict  : UB
```

Miri 메시지 읽는 법: "무엇이"(`&u8` 값을 만들었는데), "왜 잘못인가"(해제된 메모리를 가리키는 댕글링 참조), "어디서"(33번 줄 10열)의 순서다. 머리말의 `command` 줄을 그대로 복사해 터미널에 붙이면 같은 실행을 재현할 수 있다.

`run.log`는 시각순 사건 기록이다. 병렬로 돌렸을 때 어떤 파일이 언제 겹쳐 돌았는지, 어디서 멈췄는지를 여기서 본다.

```
2026-10-09T17:05:04.4xx+09:00	BATCH_START	toolchain=nightly-2026-10-01-...	files=9	jobs=1	timeout=3s	flags=...
2026-10-09T17:05:04.5xx+09:00	PREFLIGHT	OK
2026-10-09T17:05:04.530+09:00	START	#00001	.../samples/compile_err.rs
2026-10-09T17:05:04.556+09:00	END	#00001	COMPILE_ERROR	exit=1	0.026s	.../samples/compile_err.rs
```

## 5. 내 파일 목록 만들기

목록은 경로가 한 줄에 하나씩 있는 텍스트 파일이다. 디렉터리 안의 `.rs`를 전부 모으려면:

```bash
find /path/to/cases -name '*.rs' | sort > my_list.txt
./target/release/miri-batch my_list.txt
```

규칙:

- 절대 경로를 권장한다. 상대 경로는 목록 파일이 있는 디렉터리 기준으로 해석한다.
- 빈 줄과 `#`으로 시작하는 줄은 무시한다.
- 없는 파일이 하나라도 있으면 아무것도 실행하지 않고 그 목록을 보여 준다.
- 각 파일은 `main`이 있어야 한다. 외부 crate를 쓰는 파일은 돌릴 수 없다(cargo 프로젝트가 아니라 파일 하나를 직접 컴파일하기 때문).

## 6. 병렬 실행과 제한 시간

```bash
./target/release/miri-batch my_list.txt -j 8 -t 60
```

- `-j 8`: 8개를 동시에 돌린다. Miri는 프로세스당 단일 스레드라 코어 수까지 올려도 된다. 메모리를 많이 쓰는 파일이 섞여 있으면 절반쯤이 안전하다. 끝나는 대로 다음 파일을 집어 가므로 긴 파일이 몰려도 다른 스레드가 놀지 않는다.
- `-t 60`: 파일당 60초. 넘으면 강제 종료하고 `TIMEOUT`으로 적는다. 기본은 300초다. 무한 루프 파일이 전체를 막지 않게 해 준다.

샘플 9개 기준 순차 3.4초, `-j 4`로 3.1초였다(3초짜리 타임아웃 샘플이 하한이다).

## 7. 옵션을 바꾸고 싶을 때

기본 옵션 네 개는 고정이고, 그 뒤에 `-F`로 덧붙인다. 여러 번 쓸 수 있다.

```bash
# 별칭 규칙을 Tree Borrows 로 검사
./target/release/miri-batch my_list.txt -F -Zmiri-tree-borrows

# 메모리 누수 검사 끄기 + 전체 백트레이스
./target/release/miri-batch my_list.txt -F -Zmiri-ignore-leaks -F -Zmiri-backtrace=full
```

실행 시작 시 `fn main() {}`로 사전 점검을 하므로 옵션에 오타가 있으면 파일을 하나도 돌리기 전에 이렇게 멈춘다.

```
오류: preflight 실패 (CONFIG_ERROR): 빈 프로그램 `fn main() {}` 가 통과하지 못했습니다.
명령: /home/.../bin/miri --sysroot ... -Zno-such .../preflight_empty.rs
error: unknown unstable option: `no-such`
```

주의: `-Copt-level=3`을 붙여도 Miri는 최적화를 하지 않는다(해석기라 LLVM 단계가 없다). 바뀌는 것은 overflow 검사와 debug assertion뿐이다. MIR 최적화를 켜려면 `-F -Zmir-opt-level=2`처럼 주되, Miri가 "최적화가 UB를 숨길 수 있다"고 경고하는 데는 이유가 있다. 실제로 `dangling_stack.rs`는 수준 0에서는 UB로 잡히고 수준 1 이상에서는 통과한다.

## 8. 자주 겪는 문제

| 증상 | 원인과 해결 |
| --- | --- |
| `error: unknown binary 'miri' in toolchain '...'` | 그 툴체인에 miri 컴포넌트가 없다. `rustup component add miri --toolchain <이름>`. 이름 없는 `nightly`와 날짜 붙은 `nightly-YYYY-MM-DD`는 서로 다른 툴체인이다 |
| `Miri sysroot 를 얻지 못했습니다` | `cargo +<툴체인> miri setup`을 직접 실행해 메시지를 본다. 보통 miri 미설치이거나 네트워크 문제다 |
| 전부 `UNSUPPORTED` | 파일들이 Miri가 지원하지 않는 API(네트워크, 일부 syscall)를 쓴다. 그런 파일은 Miri로 검사할 수 없다 |
| 환경 변수나 파일을 읽는 프로그램이 실패 | Miri는 기본적으로 호스트와 격리된다. `-F -Zmiri-disable-isolation` |
| 결과가 전부 `COMPILE_ERROR`이고 `couldn't find file` | 목록의 경로가 잘못됐다. 절대 경로로 바꾼다 |
| 툴체인을 올렸더니 깨짐 | 이 도구는 Miri 드라이버를 직접 호출하는데, Miri 문서는 이를 공식 지원하지 않는다고 적어 두었다. `cargo +<툴체인> miri run`이 되는지 먼저 확인하고, 안 되면 Miri 설치 문제, 되면 이 도구의 호출 방식 문제이니 [DESIGN.md](DESIGN.md)의 2절을 본다 |

## 9. 작동 원리 (짧게)

1. `rustup which miri`로 드라이버 경로를, `cargo miri setup --print-sysroot`로 Miri 전용 sysroot 경로를 얻는다.
2. 파일마다 `miri --sysroot <sysroot> <옵션> file.rs`를 직접 실행한다. cargo 프로젝트를 만들지 않으므로 파일당 0.05초 안팎이고, 공유 상태가 읽기 전용 sysroot뿐이라 몇 개를 동시에 띄워도 잠금이 없다.
3. stdout/stderr를 줄 단위로 읽으며 경과 시각을 붙이고, 제한 시간을 넘기면 프로세스를 죽인다.
4. 종료 코드와 stderr 문자열로 판정을 정해 TSV와 로그에 쓴다.

코드는 `src/main.rs` 하나이고 위에서부터 명령줄, 시각, 환경, 목록, 판정, 실행, 전체 흐름, 테스트 순서로 되어 있다.
