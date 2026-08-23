# soksak-kit-sidecar-terminal

터미널 상태 복원 사이드카가 공유하는 runtime입니다. PTY의 순서가 있는 출력·gap·크기
변경·종료 관측을 받아 공급자 mirror에 적용하고 암호화된 checkpoint를 관리합니다.

상태는 각 복원 mirror가 마지막으로 관측한 열, 행, 원본 이벤트 순서를 반환합니다.
호출자는 이 값으로 크기 변경 경로에서 처음 진행하지 않은 경계를 확인합니다.
느린 mirror나 누락된 관측은 정상으로 처리하지 않고 gap 또는 실패로 보고합니다.

Terminal sidecar owner gate는 `scripts/install_pty_release.py`로
`soksak-sidecar-pty@0.0.4`의 target별 immutable release를 설치합니다. Installer는 owner release
identity, source commit, artifact size와 SHA-256을 검사하고 regular file만 압축 해제합니다. Core
checkout을 찾거나 PTY provider를 source에서 빌드하지 않습니다.

Live handoff snapshot은 mirror paint와 절대 PTY output sequence를 원자적으로 게시합니다. 따라서
`pty.attachLease`로 이어질 때 byte를 중복 재생하거나 누락하지 않습니다.
