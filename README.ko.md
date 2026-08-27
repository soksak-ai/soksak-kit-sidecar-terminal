# soksak-kit-sidecar-terminal

터미널 상태 복원 사이드카가 공유하는 runtime입니다. PTY의 순서가 있는 출력·gap·크기
변경·종료 관측을 받아 공급자 mirror에 적용하고 암호화된 checkpoint를 관리합니다.

상태는 각 복원 mirror가 마지막으로 관측한 열, 행, 원본 이벤트 순서, 절대 출력 순서,
gap 수를 반환합니다. 호출자는 이 값으로 처음 진행하지 않은 경계를 확인합니다.
느린 mirror나 누락된 관측은 정상으로 처리하지 않고 gap 또는 실패로 보고합니다.

Terminal sidecar owner gate는 `scripts/install_pty_release.py`로
`soksak-sidecar-pty@0.0.6`의 target별 immutable release를 설치합니다. Installer는 owner release
identity, source commit, artifact size와 SHA-256을 검사하고 regular file만 압축 해제합니다.
`release.json`은 artifact를 bare `file` 이름으로 지정하며 installer는
`https://github.com/soksak-ai/soksak-sidecar-pty/releases/download/v0.0.6/<file>`에서 내려받고
`url` key가 있는 문서를 거부합니다. Core checkout을 찾거나 PTY provider를 source에서 빌드하지
않습니다.

고정된 `soksak-sidecar-pty@0.0.6`의 release document는 `url` key를 가진 채 공개되었으므로 installer는 이를
거부합니다. 핀은 `url` 없이 공개되는 첫 PTY release(0.0.8 이상)로 옮기며, 그 전까지 PTY를 설치하는 owner
gate는 그 거부로 실패합니다.

Live handoff snapshot은 mirror paint와 절대 PTY output sequence를 원자적으로 게시합니다. 따라서
`pty.attachLease`로 이어질 때 byte를 중복 재생하거나 누락하지 않습니다.
Checkpoint commit은 pane마다 thread와 process 전체에서 직렬화합니다. `(generation, sequence)`는
증가만 하므로 오래된 background write가 최신 explicit archive를 덮지 않습니다. Reader는 원자적으로
이름이 바뀐 최종 파일만 읽고 기록 중인 파일은 읽지 않습니다.
새 PTY generation은 live output을 적용하기 전에 archive를 engine에 재생해 이전 화면을 scrollback으로
보존합니다. 살아 있는 generation에 다시 붙을 때는 archive를 재생하지 않습니다.
`terminal.frame`은 viewport를 run 단위로, 해당 mirror가 실제 적용한 절대 출력 순서와 같은 lock에서
게시하므로 호출자가 요청 좌표로 렌더 진행을 추정하지 않습니다. `subscriber`마다 처음에는 전체
화면을, 이후에는 바뀐 행만 받습니다. 크기 변경·offset 변경·alternate screen 전환은 다시 전체
화면을 강제합니다. `offset`은 viewport를 history 쪽으로 넘기며 `historySize`로 clamp됩니다.
`terminal.status`는 engine의 `capabilities.hyperlinks`를 보고합니다.

## 검증

```sh
make verify
```

정확한 toolchain 정본은 `rust-toolchain.toml`과 `.python-version`입니다. Make는 dependency를
준비하기 전에 version과 architecture 불일치를 거부하고 Rust suite와 PTY release installer
suite를 모두 실행합니다. 릴리스 Actions도 release train이 URL과 SHA-256으로 전달한 immutable
spec package를 주입한 뒤 같은 명령을 사용하며 spec source를 checkout하거나 다시 빌드하지
않습니다.
