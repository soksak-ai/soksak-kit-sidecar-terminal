# soksak-kit-sidecar-terminal

터미널 상태 복원 사이드카가 공유하는 runtime입니다. PTY의 순서가 있는 출력·gap·크기
변경·종료 관측을 받아 공급자 mirror에 적용하고 암호화된 checkpoint를 관리합니다.

상태는 각 복원 mirror가 마지막으로 관측한 열, 행, 원본 이벤트 순서, 절대 출력 순서,
gap 수를 반환합니다. 호출자는 이 값으로 처음 진행하지 않은 경계를 확인합니다.
느린 mirror나 누락된 관측은 정상으로 처리하지 않고 gap 또는 실패로 보고합니다.

모든 service는 process 경계에서 control contract의 `SOKSAK_PROCESS_LABEL`을 한 번 검증하고 protocol 2
announcement와 greeting에 게시합니다. 이 label은 공개 process inventory와 monitoring 전용이며 component
identity, socket, dependency, ownership 또는 executable에서 도출되는 운영체제 process 이름을 바꾸지 않습니다.

Live handoff snapshot은 mirror paint와 절대 PTY output sequence를 원자적으로 게시합니다. 따라서
`pty.attachLease`로 이어질 때 byte를 중복 재생하거나 누락하지 않습니다.
Checkpoint commit은 pane마다 thread와 process 전체에서 직렬화합니다. `(generation, sequence)`는
증가만 하므로 오래된 background write가 최신 explicit archive를 덮지 않습니다. Reader는 원자적으로
이름이 바뀐 최종 파일만 읽고 기록 중인 파일은 읽지 않습니다.
새 PTY generation은 live output을 적용하기 전에 archive를 engine에 재생해 이전 화면을 scrollback으로
보존합니다. Fresh shell이 archive의 visible row를 덮거나 이전 cursor를 물려받지 않도록 live output
전에 viewport 하나를 전진시키고 새 viewport를 지운 뒤 cursor를 home으로 옮깁니다. 살아 있는 generation에
다시 붙을 때는 archive를 재생하지 않습니다.
`terminal.frame`은 viewport를 run 단위로, 해당 mirror가 실제 적용한 절대 출력 순서와 같은 lock에서
게시하므로 호출자가 요청 좌표로 렌더 진행을 추정하지 않습니다. `subscriber`마다 처음에는 전체
화면을, 이후에는 바뀐 행만 받습니다. 크기 변경·offset 변경·alternate screen 전환은 다시 전체
화면을 강제합니다. `offset`은 viewport를 history 쪽으로 넘기며 `historySize`로 clamp됩니다.
`terminal.status`는 engine의 `capabilities.hyperlinks`를 보고합니다.

Mirror는 engine이 소유한 cursor shape와 blink 상태를 terminal state로 게시합니다. Provider/user가
설정한 blink interval은 별도 animation policy로 게시합니다. Warm rehydrate는 DECSCUSR를 복원하지만
animation phase나 policy를 직렬화하지 않습니다. 공통 native painter는 선언된 terminal theme의 `cursor`와
`cursorAccent` 색으로 block, underline, bar를 그리며 adapter는 CSI를 다시 parse하지 않습니다.
Condition variable은 engine이 visible cursor의 blinking을 선언한 동안에만 deadline을 가집니다.
Steady 또는 hidden cursor는 명시적인 output/control event만 기다리고 output activity가 blink phase를
reset합니다. Cursor 상태는 frame과 암호화 checkpoint에 들어가며 shape가 없던 옛 checkpoint 형식을
compatibility path로 받아들이지 않습니다.

Native painter는 host가 명시한 `light|dark` base palette 위에 engine의 OSC 4/10/11/12 상태를
해소합니다. null override는 terminal override가 없다는 뜻이므로 OSC 104/110/111/112 reset 뒤에는
예전에 저장한 theme가 아니라 현재 base가 드러납니다. Effective palette가 바뀌면 모든 행을
무효화하고 적용 frame을 전진시킨 뒤 `surface.state`가 `themeMode`, `baseTheme`,
`terminalOverrides`, `effectiveTheme`을 게시합니다. Provider는 engine color state를
`TerminalStateMirror.theme_overrides`로 공개하며 adapter는 OSC를 다시 parse하지 않습니다.
`surface.theme`은 완전한 replacement base 하나를 검증하고 active engine override를 보존하며
resize palette를 갱신한 뒤 render thread를 깨웁니다. Surface를 다시 열거나 polling하지 않습니다.

## 검증

```sh
make lock
make verify
```

`make lock`은 변경된 `Cargo.toml`을 기존 dependency resolution을 유지한 채 `Cargo.lock`에 투영하는
owner 연산입니다. 일반 build와 verify는 계속 `--locked`로 실행합니다.

정확한 toolchain 정본은 `rust-toolchain.toml`과 `.python-version`입니다. Make는 dependency를
준비하기 전에 version과 architecture 불일치를 거부하고 Rust suite를 실행합니다. 릴리스 Actions도
release train이 URL과 SHA-256으로 전달한 immutable
spec package를 주입한 뒤 같은 명령을 사용하며 spec source를 checkout하거나 다시 빌드하지
않습니다.
