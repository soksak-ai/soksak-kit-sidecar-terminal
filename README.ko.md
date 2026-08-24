# soksak-kit-sidecar-terminal

터미널 상태 복원 사이드카가 공유하는 runtime입니다. PTY의 순서가 있는 출력·gap·크기
변경·종료 관측을 받아 공급자 mirror에 적용하고 암호화된 checkpoint를 관리합니다.

상태는 각 복원 mirror가 마지막으로 관측한 열, 행, 원본 이벤트 순서, 절대 출력 순서,
gap 수를 반환합니다. 호출자는 이 값으로 처음 진행하지 않은 경계를 확인합니다.
느린 mirror나 누락된 관측은 정상으로 처리하지 않고 gap 또는 실패로 보고합니다.

Terminal sidecar owner gate는 `scripts/install_pty_release.py`로
`soksak-sidecar-pty@0.0.6`의 target별 immutable release를 설치합니다. Installer는 owner release
identity, source commit, artifact size와 SHA-256을 검사하고 regular file만 압축 해제합니다. Core
checkout을 찾거나 PTY provider를 source에서 빌드하지 않습니다.

Live handoff snapshot은 mirror paint와 절대 PTY output sequence를 원자적으로 게시합니다. 따라서
`pty.attachLease`로 이어질 때 byte를 중복 재생하거나 누락하지 않습니다.
`terminal.frame`은 frame과 해당 mirror가 실제 적용한 절대 출력 순서를 같은 lock에서
게시하므로 호출자가 요청 좌표로 렌더 진행을 추정하지 않습니다.

## 검증

```sh
make verify
```

정확한 toolchain 정본은 `rust-toolchain.toml`과 `.python-version`입니다. Make는 dependency를
준비하기 전에 version과 architecture 불일치를 거부하고 Rust suite와 PTY release installer
suite를 모두 실행합니다. 릴리스 Actions도 release train이 URL과 SHA-256으로 전달한 immutable
spec package를 주입한 뒤 같은 명령을 사용하며 spec source를 checkout하거나 다시 빌드하지
않습니다.
