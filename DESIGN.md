# 설계 메모

**핵심 결정은 세 가지다. cargo를 거치지 않고 Miri 드라이버를 직접 호출한다. 실행 설정은 MIR 최적화를 끈 하나만 둔다. 판정은 종료 코드가 아니라 stderr 문자열로 한다.** 각각의 근거를 측정값과 함께 적는다. 측정 환경은 Linux x86_64 24코어, nightly-2026-10-01(rustc 1.101.0-nightly), miri 0.1.0이다.

## 1. 왜 cargo를 거치지 않는가

파일당 소요 시간을 세 방식으로 쟀다(작은 프로그램 기준).

| 방식 | 파일당 | 비고 |
| --- | --- | --- |
| 파일마다 `cargo init` 후 `cargo miri run` | 0.21초 | `cargo init` 자체는 0.02초 |
| cargo 프로젝트 하나를 재사용하며 `main.rs`만 교체 | 0.12초 | cargo 고정 비용만 절약 |
| Miri 드라이버 직접 호출 | 0.04초 | 12개 동시 실행 시 전체 0.07초 |

`cargo miri run`이 느린 이유는 구조에 있다. cargo가 "컴파일"이라고 부르는 단계가 만드는 산출물은 실행 파일이 아니라 rustc 인자를 담은 JSON이고, "실행" 단계에서 `cargo-miri runner`가 그 인자로 Miri 드라이버를 다시 띄워 **소스를 그 시점에 다시 컴파일하고 해석**한다. 즉 cargo 경로에서는 소스가 사실상 두 번 컴파일된다. 드라이버를 직접 부르면 한 번이다. 표준 라이브러리는 어느 경로든 `~/.cache/miri`의 공유 sysroot(약 112MB)를 쓰고, 프로젝트 안에서 다시 빌드되지 않는다.

직접 호출의 조건과 한계:

- `--sysroot`를 반드시 넘겨야 한다. `MIRI_SYSROOT` 환경 변수는 이 버전에서 무시된다. 경로는 `cargo miri setup --print-sysroot`로 얻는다.
- 드라이버는 현재 디렉터리에 아무것도 쓰지 않는다. 예외는 컴파일러가 죽었을 때(ICE) 남기는 `rustc-ice-*.txt`인데 `RUSTC_ICE` 환경 변수로 위치를 지정한다.
- crate 이름은 항상 `--crate-name`으로 명시한다. rustc는 파일 이름에서 crate 이름을 만드는데 `test.rev1.fixed`나 `with space.rs`처럼 `.`이나 공백이 들어가면 `invalid character in crate name`으로 실패한다(확인함). 영숫자와 `_` 외의 문자는 `_`로 바꾼다.
- 드라이버는 자식 프로세스를 만들지 않는다(실행 중 `ps --ppid`로 확인). 그래서 타임아웃 시 드라이버 하나만 죽이면 된다.
- `rustup run` 없이 `rustup which miri`가 준 경로를 직접 실행해도 된다. 바이너리의 rpath가 같은 툴체인의 `lib/`를 가리킨다.
- Miri README는 드라이버 직접 호출을 "지원하지 않음"이라고 적어 두었다. 툴체인을 올렸을 때 깨질 수 있으므로 실행 시작 시 `fn main() {}` 사전 점검을 두었다.

## 2. 왜 설정이 하나인가, 왜 `-Zmir-opt-level=0`인가

여러 설정(polonius, next-solver, edition, Tree Borrows, 최적화, 레이아웃 무작위화)을 검토했고 처음에는 "최적화 0과 3" 두 설정을 두려 했다. 그러나 **Miri는 `-C opt-level`을 무시한다.** `-Copt-level=3`을 주면 이런 경고가 나온다.

```
warning: Miri does not support optimizations: the opt-level is ignored. The only effect of selecting
a Cargo profile that enables optimizations (such as --release) is to apply its remaining settings,
such as whether debug assertions and overflow checks are enabled.
```

해석기라 LLVM 최적화 단계가 없다. `-C opt-level`이 바꾸는 것은 overflow 검사와 debug assertion뿐이다. Miri가 보는 코드를 실제로 바꾸는 것은 MIR 최적화 수준 `-Zmir-opt-level`이고, Miri의 기본값은 0이다(일부러 끈다). 1 이상을 주면 Miri가 "최적화가 UB를 숨길 수 있다"고 경고하며, 실제로 그렇다.

| 파일 | `-Zmir-opt-level=0` | 1 이상 |
| --- | --- | --- |
| `samples/dangling_stack.rs` | UB (해제된 스택 메모리 읽기) | 통과(exit 0). 최적화가 읽기를 없앰 |
| `samples/issue_163886.rs` | UB, 33:10에서 보고 | UB, 21:19에서 보고 |

검사 도구의 목적은 UB를 최대한 찾는 것이므로, Miri가 보는 그대로의 수준 0 하나로 통일했다. 다른 조건이 필요하면 `-F`로 덧붙인다.

## 3. 공통 옵션이 정말 nightly 기본값인가

세 옵션을 명시하는 이유는 툴체인이 바뀌어도 같은 조건을 보장하고 로그의 명령줄만으로 조건을 알 수 있게 하기 위해서다. 기본값 여부는 직접 확인했다.

| 옵션 | 이 nightly의 기본값인가 | 확인 방법 |
| --- | --- | --- |
| `-Zpolonius=next` | 예 | `rustc -Z help`가 `polonius=val ... (default: next)`를 출력. 소스(`rustc_session/src/config.rs`)에서 `CFG_DEFAULT_POLONIUS_NEXT` 빌드 변수로 켜짐. NLL이 거부하는 고전 사례(`get_default`)가 플래그 없이 컴파일됨 |
| `-Znext-solver=globally` | 예 | `RUSTC_LOG=rustc_next_trait_solver=info`로 로그 줄 수 비교: 플래그 없음 226줄 = `globally` 226줄, `coherence` 24줄. 추적 이슈 rust-lang/rust#160895(2026-08)가 nightly 기본 활성이라고 명시 |
| `--edition=2024` | 아니오 | bare `rustc`의 기본 edition은 2015(`gen`이 키워드가 아님을 확인). cargo가 새 프로젝트에 2024를 써 줄 뿐이다 |

## 4. 판정 규칙

종료 코드 실측:

| 상황 | 종료 코드 |
| --- | --- |
| Miri UB 검출 | 1 |
| 컴파일 오류 | 1 |
| 메모리 누수 | 1 |
| 옵션 오타 | 1 |
| 컴파일러 패닉(ICE) | 101 |
| 프로그램 `panic!` | 101 |
| `process::exit(7)` | 7 |
| 타임아웃(강제 종료) | 없음 |

코드가 겹치므로 stderr 문자열을 먼저 보고, 그다음 종료 코드를 본다. 규칙은 `classify` 함수와 단위 테스트에 있다. 우선순위: TIMEOUT > ICE > CONFIG_ERROR > UB > LEAK > UNSUPPORTED > COMPILE_ERROR > PANIC > OK > EXIT_n.

## 5. 검토했으나 채택하지 않은 것

- **여러 설정의 전체 조합(축 6개, 144개)**: 시간은 문제가 아니지만(144 × 0.04초 ≈ 6초) 셀이 너무 많아 읽을 수 없다. 설정을 하나로 줄이면서 이 논의는 사라졌다.
- **한 cargo 프로젝트의 `src/bin/`에 여러 파일**: 동시 실행 시 cargo 빌드 디렉터리 잠금으로 컴파일이 직렬화된다. 바이너리 이름 제약도 있다.
- **cargo 프로젝트 재사용 + 워커 디렉터리 N개**: 직접 호출보다 3배 느리고, 옵션이 바뀌면 cargo가 다시 컴파일해 캐시 관리가 복잡해진다.
- **Python 구현**: 먼저 프로토타입을 Python으로 만들어 설계를 검증했다. 최종본은 Rust 사용자가 그대로 읽고 고칠 수 있도록 외부 crate 없는 Rust로 다시 썼다.
- **`-Zpolonius=legacy`**: 163886.rs에서 ICE(`region_infer/region_context.rs:569`)가 난다. 폐기 예정 경로라 제외했다.
