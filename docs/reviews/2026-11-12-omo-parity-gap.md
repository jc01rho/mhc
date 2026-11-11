# omo 최신 대비 mhc 격차와 이식 기록 (2026-11-12)

## 기준

| 대상 | 커밋 | 비고 |
|---|---|---|
| 포크 jc01rho/mhc main | `9356f7030` | 원본 Indosaram/mhc main `483f1490e`(2026-11-11 16:40 KST)를 동기화 작업이 머지한 상태, 포크 고유 커밋 12개 |
| 원본 Indosaram/mhc main | `483f1490e` | 포크에 이미 모두 들어 있다(`git rev-list fork/main..origin/main` = 0) |
| omo 이식 고정(PINS.md) | `354bcf9fc` | oh-my-openagent v5.1.52 |
| omo 최신 | `04e22c0b6` | oh-my-openagent dev, 2026-11-11 23:53 KST. 최신 릴리스 v5.1.69 |

## 방법

1. omo `354bcf9fc..origin/dev`의 first-parent 커밋 310건(PR 머지 단위)마다 바뀐 파일을 모은다.
2. mhc에 대응 코드가 없는 패키지(omo-opencode, omo-codex, device-service-core, omo-computer-*, senpi-desktop-*, web, omo-native 설치기)와 테스트·변경 기록·문서 파일을 빼면 mhc 관련 PR은 146건이다.
3. 146건을 영역과 내용으로 판정했다. 이식 대상으로 고른 14건은 mhc 대응 코드를 먼저 읽어 이미 같은 동작인지 확인한 뒤 옮겼다.

생성 스크립트와 판정 근거 원본은 작업 장비의 `~/.cache/omo-parity/`(inventory.py, gen_doc.py, overrides.json)에 있다.

## 원본 mhc의 방향과 이 포크에 주는 영향

- 원본 mhc는 omo 이식 고정을 v5.1.52에서 올리지 않았고, `docs/cleanroom/phase4-5-inventory.md`에 따라 omo(SUL-1.0) 크레이트를 MIT 클린룸 구현으로 갈아타는 중이다. 계획된 5단계는 `crates/omo/**` 전체와 `omo` 기능 플래그 삭제다.
- 그래서 v5.1.52 이후 omo 변경은 원본 mhc에 거의 들어오지 않는다. 이 포크가 omo 기능을 따라가려면 직접 옮겨야 한다.
- 원본이 5단계 삭제를 머지하면 이 포크의 동기화 작업(`scripts/sync-upstream.sh`)은 여기서 고친 `crates/omo` 파일과 충돌해 멈춘다. 그 시점에 omo 크레이트를 포크에 남길지, 원본을 따라 MIT 구현으로 넘어갈지 정해야 한다.
- 내장 스킬(`assets/omo-latest`)은 frontend 탐지 스크립트를 빼면 클린룸 본문이다. omo 스킬 본문 수정은 같은 문장이 mhc에 없으므로 바이트로 옮기지 않았다.
- omo 코드는 SUL-1.0이다. `jc01rho/mhc`는 공개 저장소이므로 이번에 옮긴 코드도 기존 omo 이식분과 같은 자리에 두고(`crates/omo/**`), `LICENSES/SUL-1.0.md`와 저작권 고지를 그대로 유지한다. SUL-1.0은 내부 업무용 사용·수정과 무상 비상업 배포를 허용하고 고지 제거를 금지한다.

## 결과 요약

| 판정 | 건수 |
|---|---:|
| 이식함 | 12 |
| 이미 같은 동작 | 1 |
| 이식 불가(바탕 기능이 mhc에 없음) | 3 |
| 미이식(후순위) | 7 |
| 미이식(대형) | 4 |
| 주인 결정 필요 | 4 |
| 스킬 본문(클린룸이라 바이트 이식 대상 아님) | 24 |
| 해당 없음 / 부분 해당 없음 | 93 |

이식 커밋 12개는 PR 하나당 커밋 하나다. 각 커밋에는 omo 테스트를 옮긴 Rust 테스트가 있고, 수정을 되돌리면 새 테스트가 실패하는 것을 확인했다.

## 이식한 변경

| omo PR | 커밋 | 바뀐 동작 |
|---|---|---|
| #11946 | `441a0207b` | 하위 작업 시작 정보를 기록 잠금 안에서 써서, 동시에 발급된 입력 번호(effect_seq)가 덮어써지지 않는다 |
| #11687 | `a49706fb8` | 분리된 stdio RPC 클라이언트는 진행 중 요청을 즉시 실패시키고 이후 요청을 쓰지 않는다. 정리된 자식의 타이머가 남지 않는다 |
| #7662 | `3df3a7ab5` | 같은 호출자가 바뀌지 않은 하위 작업을 `task_output` status로 다시 조회하면 전체 스냅샷 대신 `no_progress`와 짧은 이유를 받는다. `omo.task.output` RPC도 같다 |
| #12268 | `d80663a00` | Resume이 기록된 Stop의 순번으로 일시정지된 자식을 재개한다. 복구하지 못한 자식은 5초·30초·120초에 다시 시도한 뒤 `session_unavailable` 오류로 끝난다. `/task-kill` 목록과 하단 표시줄에 일시정지된 자식이 나온다 |
| #12343 | `8e055f7b0` | 놓친 Resume 때문에 Stop이 남아 있으면 다음 턴 시작 때 엔진 상태를 보고 풀어 준다. 이전 프로세스가 남긴 Stop은 풀지 않는다(`SAFETY-DIVERGENCE` 표시) |
| #12097 | `061b56dfb` | 메모리가 실행하는 모든 git 명령에 `-c core.fsmonitor=false`를 붙인다 |
| #11945 | `9ac97b2af` | 출력 없이 실패한 git 자식이 어떤 시그널로, 몇 ms 뒤에 끝났는지 오류에 남긴다 |
| #11869 | `8c1378f47` | 스킬 MCP 서버 설정의 `timeout`(ms)이 목록·호출 요청에 적용된다. 없거나 잘못된 값이면 기존 30초 |
| #12397 | `a86549807` | 설정 감시가 새 디렉터리를 기다리는 범위를 `.git`이 있는 프로젝트 루트까지로 줄인다. HOME과 `/`는 감시하지 않는다 |
| #12636 | `0b0a9bcc9` | 맡길 모델 체인을 모두 쓸 수 없으면 체인의 모든 모델, 필요한 제공자, 고칠 설정 키(`categories.<이름>.model` / `agents.<이름>.model`)를 오류에 적는다. 자식은 띄우지 않는다 |

## 이식하지 못한 것과 이유

- #12378(bwrap 점검 시간 초과 후 샌드박스가 꺼진 채 남는 문제): mhc에는 bwrap이 실제로 동작하는지 점검하는 단계(omo `sandbox-bwrap-probe.ts`)가 아예 없다. 코드상으로는 bwrap이 설치돼 있으면 항상 샌드박스로 감싸므로, 사용자 네임스페이스가 막힌 컨테이너에서는 메모리 반성 실행이 실패할 수 있다(실측은 하지 않았다). 점검 기능을 먼저 이식해야 한다.
- #12815(git 시간 초과 프로세스 스냅샷): 스냅샷 기능(omo `timeout-diagnostics.ts`) 자체가 mhc에 없다.
- #11591(type "sse" 스킬 MCP에 예전 HTTP+SSE 전송 사용): mhc MCP 클라이언트는 Stdio와 Streamable HTTP만 있다(`crates/builtins/maho-ext-mcp/src/transport_sdk.rs`). 예전 SSE 전송을 새로 구현해야 한다.

## 주인 결정이 필요한 것

- #12010, #12761: 하위 작업이 지정 모델 체인으로만 돌고, 부모 모델로 대체 실행하던 경로를 없앤다. 대체 모델 정책이 바뀐다.
- #12239: architect 카테고리가 직접 구현·디버깅하도록 바뀐다. 설계 제안 위주로 쓰는 지금 설정과 맞지 않는다.
- #7593: eval-integrity 감사 스킬. 쓸 때마다 토큰이 더 든다.

## 미이식(대형)

- #12517(반성 잠금 회수기 통합), #12532(메모리 런타임 보존 기간 정리), #12538(정체성별 2GiB 상한): 각각 수천 줄이고, 원본 mhc가 같은 문제를 자체 구현으로 고치는 중이다(`0ade0c11e`, `b91e1da13`, `d87de00df`). 그대로 옮기면 원본과 충돌할 가능성이 크다.
- #12839(자식 대화상자를 부모 화면으로, 승인 모드 상속): 대화형 화면 중심 기능이다.

## 검증

빌드와 테스트는 devsecops에서 했다(작업 장비 teamcity에는 C 컴파일러가 없다). 묶음별 결과와 통합 트리 결과는 PR 본문에 적는다.

이미 원본 main에서도 실패하던 테스트:

- `maho-senpi-task` lib 테스트를 한 프로세스에서 모두 돌리면 작업 ID 상한 테스트가 프로세스 전역 상한을 소진해 뒤 테스트가 실패한다. 크레이트는 테스트마다 프로세스를 따로 쓰는 nextest를 전제한다.
- `maho-memory-core` 2건, `maho-omo-memory` 2~3건(잠금 소유 판정·잠금 브로커). 원본 main에서도 같은 테스트가 실패한다.
- `cargo fmt --all -- --check`의 차이는 원본 main에도 있는 파일들뿐이다. 이식 커밋이 새로 만든 서식 차이는 없다.

## 전체 판정표

판정 열의 커밋은 이 브랜치의 이식 커밋이다.

| 날짜 | PR | 제목 | 영향 영역(파일 수) | 판정 | 근거 |
|---|---|---|---|---|---|
| 2026-10-30 | #11837 | fix(frontend): keep the detect probe after the doctype and pin the capture notice | skill:frontend(5) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-10-30 | #11591 | fix(mcp-client-core): use the legacy HTTP+SSE transport for type "sse" skill MCPs | mcp-client-core(3) | 이식 불가 | mhc MCP 클라이언트에 예전 HTTP+SSE 전송이 없음(maho-ext-mcp transport_sdk.rs는 Stdio·Http만). 새 전송 구현이 선행 과제 |
| 2026-10-30 | #11842 | Batch #21: bundle-train car (12 PRs) — 포함: #11665, #11545, #10898 | memory-core(9), model-core(1), omo-senpi/feedback(1), omo-senpi/formatter(1), omo-senpi/lsp(1), omo-senpi/memory(7), omo | 부분 해당 없음 | 묶음 머지. 포함 PR 대부분 문서·macOS 자동마운트·변경 기록 |
| 2026-10-30 | #11794 | fix(claim-receipt): count bun check as typecheck evidence | omo-senpi/claim-receipt(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-10-30 | #11774 | feat(memory): enable the first sync device through the genesis bridge | memory-core(23) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-10-31 | #11918 | Merged by merge-gate; cleared pre-existing failures: test (#11526); CI_PREEXISTING_CLEARED test (windows-latest, 1/2) issue=#11943 tests=[packages\mem | omo-senpi/memory(3), senpi-task(1) | 해당 없음 | merge-gate 테스트 정리 묶음 |
| 2026-10-31 | #11888 | Batch #22: bundle-train car (4 PRs) | omo-senpi/telemetry(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-10-31 | #11894 | batch #23: bundle train for 5.1.53 — 포함: #11783, #11807, #11802, #11737, #11765, #11526, #11262, #11580 | memory-core(4), omo-senpi/computer-use(3), omo-senpi/feedback(1), omo-senpi/memory(28), omo-senpi/task(8), omo-senpi/src | 부분 해당 없음 | 5.1.53 묶음. 메모리 동기화 RPC·릴리스 도구·테스트 위주 |
| 2026-10-31 | #11970 | feat(programming): scaffolders run on Bun in strict TypeScript | skill:programming(14) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-10-31 | #11967 | docs(programming): route test files to a testing reference carrying the authoring gate and junk classes | skill:programming(4) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-10-31 | #11991 | fix(programming): the go scaffolder writes LF on a CRLF checkout | skill:programming(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-10-31 | #11972 | feat(programming): check-test-junk detector and mutation-proof helper on Bun | skill:programming(5) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-01 | #11869 | fix(mcp-client-core): propagate the configured request timeout to skill MCP calls (#6631) | claude-code-compat-core(1), mcp-client-core(2) | 이식 8c1378f47 | 스킬 MCP 서버 설정 timeout(ms)을 목록·호출 요청에 적용, 없으면 기존 30초 |
| 2026-11-01 | #11968 | docs(remove-ai-slops): treat tests as a cleanup surface with the junk-class table and proof rule | skill:remove-ai-slops(1), skills-loader-core(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-01 | #11882 | fix(telemetry): stop OTLP after an install-key rejection | omo-senpi/telemetry(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-01 | #12017 | remove-ai-slops: name the programming skill's detector and mutation-proof scripts | skill:remove-ai-slops(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-01 | #11909 | feat(memory): name the parked-facts cause and run log in /doctor; report retry outcomes | omo-senpi/memory(4) | 미이식(후순위) | /doctor에 보류 사실 원인 표시(진단 출력) |
| 2026-11-01 | #11905 | feat(task): add per-spawn model distribution for categories and agents | model-core(1), omo-config-core(4), omo-senpi/task(1), senpi-task(7) | 미이식(후순위) | 카테고리·에이전트별 모델 분산 옵션(새 설정 키). 기능 추가라 별도 작업 |
| 2026-11-01 | #11936 | fix(ulw-research): validate kept raw bytes before replacement | skill:ulw-research(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-01 | #11945 | fix(memory-core): name how a git child ended when it fails without output | memory-core(3) | 이식 9ac97b2af | 출력 없이 실패한 git 자식의 시그널·경과 시간을 오류에 기록 |
| 2026-11-01 | #11946 | fix(senpi-task): effect_seq survives the spawn-facts stamp and same-size record rewrites | senpi-task(2) | 이식 441a0207b | spawn facts 기록을 record 잠금 안의 mutate로 바꿔 동시에 발급된 effect_seq가 사라지지 않게 함 |
| 2026-11-01 | #12001 | fix(memory-sync): recover the CLI account after a transient null credential read | omo-senpi/memory(1) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-01 | #12037 | train: brain/hand omo train (#11954 #11957 #11985 #11986 #11935 #11921 #11693) — 포함: #11691 | omo-senpi/desktop-toolkit(5), omo-senpi/memory(9), omo-senpi/src:extension(2), senpi-task(3) | 부분 해당 없음 | brain/hand(데스크톱) 묶음 + #11691 진행 부분에 시작 정보 유지 |
| 2026-11-02 | #12062 | fix(telemetry): turn span role and closed OTLP strings stay inside the collector vocabulary | omo-senpi/telemetry(4) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-02 | #11983 | chore: adopt senpi 2026.10.31-2 | omo-senpi/memory(3), omo-senpi/telemetry(1), senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-02 | #12071 | fix(dev-browser): release the CDP port with SIGTERM before server.sh can kill -9 it | skills-loader-core(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-02 | #11931 | fix(senpi-task): detach an unacknowledged session close | senpi-task(1) | 이미 있음 | mhc는 close 중에도 client를 비우지 않고 detach가 같은 client를 멈춘다(senpi-task session_client_connection.rs:11, maho-cli task_host_connection.rs:214-224) |
| 2026-11-02 | #12068 | fix(ultimate-browsing): give a timed-out template's Chrome a graceful quit | skill:ultimate-browsing(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-02 | #12010 | fix(senpi-task): a child runs only on its designated models; remove the parent-model fallback | omo-config-core(1), omo-senpi/task(2), senpi-task(46) | 주인 결정 | 자식 작업이 부모 모델로 대체 실행되던 경로를 없앤다. 대체 모델 정책이 바뀌므로 주인 판단이 필요 |
| 2026-11-02 | #12126 | chore: adopt senpi 2026.11.2-1 | senpi-task(2) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-02 | #12094 | Raise every built-in medium/xhigh reasoning default to high | model-core(2), omo-senpi/model-profile(1), senpi-task(3) | 해당 없음 | #12272에서 바로 되돌려져 순효과 없음 |
| 2026-11-03 | #12152 | fix(memory-sync): validate received item keys before they reach the filesystem or git | memory-core(11) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-03 | #12160 | fix(omo-senpi): drop finished team members from the task widget | omo-senpi/task(2) | 해당 없음 | TUI 작업 위젯 표시(헤드리스 워커와 무관) |
| 2026-11-03 | #12097 | fix(memory-core): disable fsmonitor for every memory git command | memory-core(1) | 이식 061b56dfb | 메모리 git 명령 전부에 -c core.fsmonitor=false |
| 2026-11-03 | #7662 | fix(senpi-task): stop replaying unchanged task_output snapshots | omo-senpi/task(5), senpi-task(4) | 이식 3df3a7ab5 | 같은 호출자의 무변화 task_output status 조회는 no_progress로 짧게 답함(도구와 omo.task.output RPC 모두) |
| 2026-11-03 | #12040 | Fix #11570: tokenize SVG foreignObject in fact verify | skill:ulw-research(3) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-03 | #7593 | feat(skills): add evaluation integrity audit | omo-senpi/telemetry(3), skill:eval-integrity(1) | 주인 결정 | eval-integrity 감사 스킬 추가(실행 시 추가 토큰). 지난 검토에서도 주인 결정으로 남김 |
| 2026-11-03 | #12201 | Merged by merge-gate; cleared pre-existing failures: dev-browser standalone setup releases the CDP port > with no grace window it still runs cleanly u | memory-core(1) | 해당 없음 | dev-browser 테스트 정리 묶음 |
| 2026-11-03 | #12200 | chore: adopt senpi 2026.11.2-3 (first-message stall fix, kill guard) | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-03 | #12122 | fix(omo-senpi): publish the DAG snapshot on attach before the first turn | omo-senpi/task(5), senpi-task(3) | 미이식(후순위) | DAG 스냅샷을 첫 턴 전에 게시. 화면 표시용이라 헤드리스 우선순위 낮음 |
| 2026-11-03 | #12239 | fix(senpi-task): let the architect category implement and debug broadly | senpi-task(2) | 주인 결정 | architect 카테고리가 직접 구현하게 바뀐다. 주인 설정의 architect 역할(설계 제안 위주)과 충돌 |
| 2026-11-04 | #12272 | revert: restore pre-#12094 reasoning defaults (daily Opus back to medium) | model-core(2), omo-senpi/model-profile(1), senpi-task(3) | 해당 없음 | #12094 되돌림(순효과 없음) |
| 2026-11-04 | #12292 | fix(memory-sync): accept the contract batch_id on /v1/sync/entries batch rows — 포함: #12277 | memory-core(1) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-04 | #11937 | Fixes #11934 | senpi-task(1) | 해당 없음 | 테스트용 가짜 시계가 다른 작업의 타이머를 건드리지 않게 하는 테스트 픽스처 수정 |
| 2026-11-04 | #12233 | test: fire captured timers from a snapshot so no fake-timer helper can spin forever | senpi-task(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-04 | #12311 | chore: adopt senpi 2026.11.3-3 | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-04 | #12268 | fix(senpi-task): a run Resume reaches the children its Stop paused; unreachable paused children end instead of staying paused | omo-senpi/task(4), senpi-task(4) | 이식 d80663a00 | Resume이 기록된 Stop seq로 자식을 재개, 복구 못한 자식은 5/30/120초 재시도 후 session_unavailable로 종료 |
| 2026-11-04 | #12291 | perf(omo-senpi): store the shared ultrawork directive text once (-27.9 KB omo.js) | omo-senpi/ultrawork(1) | 해당 없음 | JS 번들 크기 최적화 |
| 2026-11-04 | #12341 | test(senpi-task): keep the 32-process store-index test off disk flush latency | senpi-task(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-04 | #11530 | feat(desktop-macos): keep the display awake while operating and say when it sleeps | omo-senpi/computer-use(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-04 | #12324 | feat(omo-senpi): thread and subagent tools declare runs: "brain" for split sessions | omo-senpi/task(3), omo-senpi/thread(2) | 해당 없음 | brain/hand 분리 세션(데스크톱·원격) 표기. mhc 헤드리스 워커에는 분리 세션이 없다 |
| 2026-11-04 | #12302 | fix(memory-core): rotate the account key when a sync member is revoked or signs out (#12280) | memory-core(15), omo-senpi/memory(8) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-05 | #12343 | fix(omo-senpi): the next turn releases a Stop whose Resume the run-pause bridge missed | omo-senpi/task(3) | 이식 8e055f7b0 | 다음 턴 시작 때 엔진에 Stop이 남아 있지 않으면 놓친 Resume을 대신 실행(이전 프로세스의 Stop은 SAFETY-DIVERGENCE로 제외) |
| 2026-11-05 | #11671 | fix(memory): shutdown waits for every in-flight memory cache read (Windows EBUSY in compaction-survival teardown) | memory-core(5), omo-senpi/memory(15) | 해당 없음 | Windows EBUSY 전용 |
| 2026-11-05 | #11687 | fix(senpi-task): no timer survives a disposed stdio child (#11673) | senpi-task(4), utils(1) | 이식 a49706fb8 | detach 후 진행 중 RPC 요청을 즉시 실패시키고 이후 요청은 쓰지 않음 |
| 2026-11-05 | #12331 | fix(omo-senpi): thread chains have no hidden count limit; A->B->A is delivered | omo-senpi/thread(9) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-05 | #12079 | fix(engine-trust): trust under a bwrap user namespace via parent inode attestation | omo-senpi/memory(1) | 해당 없음 | TS 엔진 신뢰 저장소(omo-native) 전용 |
| 2026-11-05 | #12378 | fix(omo-senpi): a timed-out bwrap probe no longer disables the reflection sandbox until restart | omo-senpi/memory(3) | 이식 불가 | mhc에는 bwrap 동작 점검(sandbox-bwrap-probe.ts) 자체가 없음. 그 점검 기능 이식이 선행 과제 |
| 2026-11-05 | #12257 | feat(device-service): bootstrap the device service at every interactive omo start, with omo remote control | omo-config-core(3), omo-senpi/device-service(3), omo-senpi/src:extension(1), omo-senpi/src:install(2) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-05 | #12380 | feat(device-service): every tui and rpc engine keeps its own agent dir registered with the device service | omo-senpi/device-service(3) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-05 | #12457 | test(omo-senpi): wait for the joined wake marker on a driver line, not a recursive fs.watch | omo-senpi/thread(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-05 | #12509 | fix(omo-senpi): an applied onboarding scan stays retained so the desktop's understanding step can read it (#12508) | omo-senpi/onboarding(2) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-06 | #12500 | docs(senpi-task, lsp): make task and LSP tool descriptions match the code | omo-senpi/lsp(1), senpi-task(3) | 미이식(후순위) | task·LSP 도구 설명문을 코드와 맞춤. 설명문 차이는 mhc 쪽 확인 필요 |
| 2026-11-06 | #12303 | fix: retire the memory-sync trust pin when a device's keys are discarded (#12280) | memory-core(11), omo-senpi/memory(4) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-06 | #12484 | fix(skills): resolve dangling skill, path and agent references and gate them in CI | skill:ultrawork(1), omo-senpi/ultrawork(1), skill:browser(1), skill:frontend(6), skill:programming(4), skill:ulw-execute | 스킬 본문 | omo 스킬·ultrawork 지시문의 끊긴 참조 정리. mhc 스킬은 클린룸 본문이고 mhc에는 bun-1-4 스킬이 실제로 있어 같은 결함이 아니다 |
| 2026-11-06 | #12501 | test(senpi-task): create engine agent dirs with mode 0700 | senpi-task(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-06 | #12523 | fix(skills): resolve the Senpi-edition dangling references from the community audit | skill:ulw-loop(1), skill:ulw-plan(1), skill:frontend(3), skill:refactor(1), skill:ultimate-browsing(2), skills-loader-co | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-06 | #12598 | chore: adopt senpi 2026.11.5-1 | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-06 | #12476 | fix(memory): deny ancestor listing and search only when another memory identity exists (#12301) | omo-senpi/memory(3) | 미이식(후순위) | 다른 메모리 정체성이 있을 때만 상위 목록 조회 차단. 메모리 가드 세부 동작 |
| 2026-11-06 | #12595 | test(omo-senpi,senpi-task): keep spawned-host socket paths under the engine limit | senpi-task(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-06 | #12528 | fix(skills): resolve the context conflicts from the community skills audit | skill:dag-library(1), skill:give-me-tips(1), skill:hyperplan(1), skill:init-deep(1), skill:mass-ulw(1), skill:onboarding | 스킬 본문 | omo 스킬 본문의 상충 지시 정리. mhc 스킬은 클린룸 본문이라 바이트 이식 대상이 아니다 |
| 2026-11-06 | #12614 | fix(skills): correct the second-pass audit rows the first fold left behind | skill:dag-library(1), skill:give-me-tips(1), skill:init-deep(2), skill:mass-ulw(2), skill:ulw-loop(1), skill:ulw-plan(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-06 | #12615 | fix(skills): refactor member tools, its deep fallback, and the rg examples | skill:debugging(3), skill:refactor(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-06 | #12612 | docs(skills): teach inline-chart lessons in visualize, data-scientist and frontend | skill:visualize(1), skill:data-scientist(2), skill:frontend(1), skills-loader-core(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-06 | #11613 | feat(service-auth): creator hand-off before any keychain prompt; omo doctor helper line (#11262 PR 2d) | team-core(5) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-06 | #12636 | fix(omo-senpi): a dead model chain fails with every model and the exact fix | omo-senpi/task(2), senpi-task(2) | 이식 0b0a9bcc9 | 모델 체인이 모두 쓸 수 없을 때 체인 전체·필요 제공자·고칠 설정 키를 오류에 표시 |
| 2026-11-06 | #12335 | feat(omo-senpi): thread tools reach sessions on the account's other devices | omo-senpi/thread(17), senpi-task(1) | 해당 없음 | 계정의 다른 장비 thread 접근(원격 장비 기능) |
| 2026-11-06 | #12649 | fix(thread): ThreadCloseParams is module-internal (dev knip red) | omo-senpi/thread(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-06 | #12336 | feat(omo-senpi): thread tools reach the account's devices through the device service by default | omo-senpi/thread(12) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-06 | #12651 | fix(thread): address a session created on another device by its listing key | omo-senpi/thread(5) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-06 | #12260 | feat(desktop-core): additive protocol vocabulary for action reports, session options, ax evidence, epoch, 3 error codes | omo-senpi/telemetry(2) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-07 | #12650 | fix(omo-senpi): an endpoint lookup that fails before sending is refused, not uncertain | omo-senpi/thread(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-07 | #12258 | feat(omo-senpi): embed the bunshin-ts fleet SDK for eval cells | omo-senpi/skill-pointers(1), omo-senpi/ulw-loop(1), omo-senpi/src:extension(6), omo-senpi/src:install(2) | 해당 없음 | bunshin-ts 함대 SDK를 TS eval 셀에 내장. mhc codemode는 별도 구현 |
| 2026-11-07 | #12660 | fix(omo-senpi): confirm a remote prompt answered started without a turnId | omo-senpi/thread(2) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-07 | #12397 | fix(omo-senpi): bound config-watch creation targets at the project root | omo-senpi/config-watch(1) | 이식 a86549807 | config-watch 생성 감시를 .git이 있는 프로젝트 루트까지로 제한, HOME과 / 제외 |
| 2026-11-07 | #12568 | fix(memory-sync): refuse non-regular leaf files on landing | memory-core(1) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-07 | #12064 | test(memory): guard that the launch classification module imports nothing | omo-senpi/memory(14) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-07 | #12672 | fix(thread): one relay round trip per remote thread op | omo-senpi/thread(5) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-07 | #12545 | fix(memory-core): assert the timeout-diagnostics contract, not the host's snapshot | memory-core(1) | 해당 없음 | 테스트 계약 수정 |
| 2026-11-07 | #12679 | perf(thread): a remote thread_send is one relay round trip (no get_state before the prompt) | omo-senpi/thread(2) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-07 | #12544 | fix(omo-senpi): name the onboarding skill instead of its install path | omo-senpi/bundled-skills(1), omo-senpi/onboarding(1) | 미이식(후순위) | 온보딩 스킬 이름 표기 |
| 2026-11-07 | #12704 | fix(memory-sync): claim the desktop channel at engine boot so a host-ensure engine sends its bootstrap without a session (#12702) | omo-senpi/memory(4) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-07 | #12488 | fix(thread): a send whose wake reached no drain answers queued_offline, not queued | omo-senpi/thread(3) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-07 | #12597 | fix(memory-sync): merge train of #12295, #12299, #12298, #12360, #12297, #12104, #12105 and #12132 — 포함: #12597, #12354, #12284, #12279, #12286, #1228 | memory-core(28), omo-senpi/memory(5) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-08 | #12760 | fix(telemetry-core): share a per-install 429 backoff across processes | telemetry-core(3) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-08 | #12735 | feat(relay): hand device keys to relayd from memory, no relay keychain reads (design D PR 4, omo) | omo-senpi/memory(2) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-08 | #12517 | fix(memory): one stale reflection-lock reaper for session start, each turn and omo doctor --fix (#11976, #11920) | memory-core(18), omo-senpi/memory(16) | 미이식(대형) | 반성 잠금 회수기 통합(약 4천 줄). 원본 mhc가 같은 증상을 자체 수정 중(b91e1da13, d87de00df)이라 충돌 위험 큼 |
| 2026-11-08 | #12532 | feat(memory): bounded, resumable retention for the per-identity runtime (#12479) | memory-core(20), omo-config-core(1), omo-senpi/memory(13) | 미이식(대형) | 메모리 런타임 보존 기간 정리(약 4.5천 줄). 원본 mhc 0ade0c11e가 보존 기간 정리를 자체 구현 |
| 2026-11-08 | #12538 | feat(memory): per-identity runtime size cap with oldest-first eviction (#10144) | memory-core(14), omo-config-core(1), omo-senpi/memory(8) | 미이식(대형) | 정체성별 2GiB 상한과 오래된 것부터 축출. #12532 위에 쌓임 |
| 2026-11-08 | #12783 | fix: clear both dev reds from the memory retention cars (lint ratchet + compiled doctor parity) | memory-core(1) | 해당 없음 | lint 기준·compiled doctor(omo-native) 정리. 실제 동작 변경은 #12532 후속 |
| 2026-11-08 | #12659 | fix(device-service): stay quiet at start while the pinned release is unpublished | omo-senpi/device-service(1) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-08 | #12663 | fix(browser): the doctor never calls BrowserSkill extension 0.4.0 ready | skill:browser(3) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-08 | #12645 | fix(memory): repoint links to notes a dream run removes; never reuse an archived facts ledger path | memory-core(9), omo-senpi/memory(2) | 미이식(후순위) | dream 실행이 지운 노트 링크 재지정. 원본 mhc d4dad34f4가 깨진 링크 보고를 자체 구현 |
| 2026-11-08 | #12709 | feat(remote): omo remote opens, sends to, switches the model of, interrupts and closes sessions on other devices | omo-senpi/thread(5) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-08 | #12756 | fix(thread): a remote command on a closed session answers unknown_session, as locally | omo-senpi/thread(2) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-08 | #12764 | test(thread): pin the hop count a remote thread_send carries and b's chain_count_limit refusal (O8) | omo-senpi/thread(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-09 | #12686 | perf(thread): a remote create asks for its located session on open_session, skipping session.locate | omo-senpi/thread(4) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-09 | #12761 | fix(senpi-task): a child whose engine cannot hold its designated chain is never started | omo-senpi/computer-use(1), omo-senpi/task(1), senpi-task(18) | 주인 결정 | #12010 정책(지정 모델만)에 기대는 시작 거부. #12010과 함께 판단 |
| 2026-11-09 | #12837 | fix(omo-senpi): reach another device's thread by name and claim aliases from CLI hosts (#12833) | omo-senpi/memory(1), omo-senpi/thread(15) | 해당 없음 | 다른 장비 thread 별칭 접근(원격 장비 기능) |
| 2026-11-09 | #12847 | fix(shared-skills): stage the omowright runtime from TypeScript sources too | skill:browser(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-09 | #12858 | fix(thread): a send to another device carries the sender's gateway provenance (part of #12852) | omo-senpi/thread(8) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-09 | #12839 | feat(senpi-task): child dialogs on the parent's UI, children open in and follow its approval mode | omo-senpi/memory(2), omo-senpi/task(4), omo-senpi/thread(5), omo-senpi/src:extension(1), senpi-task(31) | 미이식(대형) | 자식 대화상자를 부모 UI로, 승인 모드 상속(31개 파일). 대화형 UI 중심 |
| 2026-11-09 | #12862 | fix(omo-senpi): senpi seam contract, absent-seam startup line, remote_wake_unavailable on monitor open | omo-senpi/src:extension(3), omo-senpi/src:senpi-test-runtime.ts(1) | 해당 없음 | senpi(TS) 호스트 seam 계약. mhc는 엔진과 확장이 한 프로세스라 해당 seam이 없다 |
| 2026-11-09 | #12878 | fix(thread): a cross-device thread_send names no device in the receiver's text | omo-senpi/thread(2) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-09 | #12795 | fix(memory): correct the stale no-host-cap comment and pin the drain budget under senpi's cap | omo-senpi/memory(3) | 해당 없음 | 주석·테스트 고정만 바뀜 |
| 2026-11-09 | #12887 | refactor(thread): one delivery pipeline with a transport port (part of #12879) | omo-senpi/thread(8) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-09 | #12471 | test: add a remaining-test-budget wait primitive and reset it in every package test root | agents-md-core(1), ast-grep-mcp(1), boulder-state(1), claude-code-compat-core(1), comment-checker-core(1), delegate-core | 해당 없음 | 테스트 대기 원시 함수(전 패키지 테스트 루트) |
| 2026-11-09 | #12832 | docs(skills): ground every ulw-research and mass-research conclusion in a tool observation | skill:mass-ulw(1), skill:ulw-research(4) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-09 | #12889 | feat(thread): cross-device send is the same delivery as a same-device send (part of #12879) | omo-senpi/thread(18) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-09 | #11706 | test(senpi-task): bound the cold-revive residency test by product liveness, relay senpi phase timings (#10993) | senpi-task(5) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-09 | #12904 | chore(senpi): adopt senpi 2026.11.9 | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-09 | #12891 | refactor(thread): one create/open/fork body over a SessionHost port (part of #12879) | omo-senpi/thread(9) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-10 | #12920 | chore(senpi): adopt senpi 2026.11.9-2 | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-10 | #12494 | docs(browser): teach BrowserSkill 0.4 shared control for the shipped omowright | skill:browser(4), skill:debugging(1), skill:ultimate-browsing(1), skill:visual-qa(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-10 | #12860 | fix(omo-senpi): omo thread send to an own alias reaches the thread from another workspace (#12833) | omo-senpi/thread(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-10 | #12890 | refactor(thread): one tool body per control tool over a SessionControl port (part of #12879) | omo-senpi/thread(18), senpi-task(1) | 해당 없음 | thread 원격 제어 리팩터(원격 장비 기능) |
| 2026-11-10 | #12911 | feat(thread): omo remote send over the transport port, one outcome mapping for every relay reply (part of #12879) | omo-senpi/thread(13) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-10 | #12921 | feat(thread): the default delivery mode steers the running turn (part of #12879) | omo-senpi/thread(3) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-10 | #12922 | fix(browser-skill): ship omowright's LICENSE beside the bundle and correct ATTRIBUTION.md | skill:browser(2) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-10 | #12953 | feat(model-core): Claude Fable 5.2 is the current Fable; retire the 5.5 launch gate | model-core(9), omo-senpi/model-profile(1), omo-senpi/telemetry(3), senpi-task(6) | 해당 없음 | Claude Fable 모델 카탈로그 변경. 주인 지시로 fable 계열은 모든 선택 경로에서 차단돼 있다 |
| 2026-11-10 | #12975 | feat(thread): a send to a peer parked on an approval is queued with waiting_on approval on that device (part of #12886) | omo-senpi/init-deep-advisor(2), omo-senpi/task(1), omo-senpi/thread(10), senpi-task(2) | 해당 없음 | 다른 장비 peer 승인 대기 큐(원격 장비 기능) |
| 2026-11-10 | #12979 | fix(service-auth): take the credential lock in the OS account's home, not $HOME (#12978) | omo-senpi/memory(1) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-10 | #13003 | chore(senpi): adopt senpi 2026.11.10 (Fable 5.2) | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-10 | #12815 | fix(memory-core): write the git-timeout process snapshot to a temp file, not a capped pipe | memory-core(8), omo-senpi/memory(1) | 이식 불가 | mhc에는 git 시간 초과 프로세스 스냅샷(timeout-diagnostics.ts) 자체가 없음 |
| 2026-11-11 | #13005 | fix(thread): share one endpoint enumeration and start its cache window when it answers (#12989) | omo-senpi/thread(1) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-11 | #13002 | fix(device-service-core): optional durable_id on listing rows asked with rows: 3 (part of #12996) | omo-config-core(1), omo-senpi/computer-use(1), omo-senpi/memory(3), omo-senpi/thread(8), omo-senpi/src:install(2) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-11 | #13000 | fix(thread): resolve a sender's durable id across devices from the relay listing (part of #12996) | omo-senpi/thread(23) | 해당 없음 | 계정 기반 메모리 동기화·원격 장비 기능. mhc에 없고 오모냥은 git 번들 병합을 쓴다 |
| 2026-11-11 | #13044 | fix(thread): the reply allowance lasts for the work a delivery started (#13042) | omo-senpi/thread(3) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-11 | #13054 | chore: adopt senpi 2026.11.10-2 | senpi-task(1) | 해당 없음 | senpi 버전 채택. mhc는 senpi를 PINS.md로 따로 추적한다 |
| 2026-11-11 | #12991 | redaction: report a registry slot conflict as restart-needed | senpi-task(1) | 해당 없음 | senpi(TS) 엔진의 프로세스 전역 redaction 레지스트리 충돌 처리. mhc 엔진 구조에는 없다 |
| 2026-11-11 | #12866 | test(senpi-task): process children in the model-chain world wait for their designated chain (#12638) | senpi-task(1) | 해당 없음 | omo 테스트 인프라만 바뀜 |
| 2026-11-11 | #13055 | Merge train F: computer-use v2 plan rows (members rebuilt on train E) | omo-config-core(1) | 해당 없음 | computer-use v2(데스크톱) |
| 2026-11-11 | #13038 | fix(thread): omo remote send on the one delivery pipeline; one steer preflight (#13016 U3) | omo-senpi/thread(17) | 해당 없음 | 원격 장비·데스크톱·컴퓨터 사용·원격 측정·대화형 UI 전용 |
| 2026-11-11 | #12714 | fix(ast-grep skill): smoke tests pass the path before --lang (#12621) | skill:ast-grep(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-11 | #13091 | feat(shared-skills): open the programming skill with code ownership | skill:programming(1) | 스킬 본문 | mhc 내장 스킬은 클린룸 본문이라 omo 바이트를 그대로 옮기는 대상이 아니다 (frontend 탐지 스크립트만 omo 바이트) |
| 2026-11-11 | #12227 | style: adopt bun fmt and reformat JS/TS source | agents-md-core(8), ast-grep-mcp(9), boulder-state(8), claude-code-compat-core(22), comment-checker-core(5), delegate-cor | 해당 없음 | 서식 일괄 변경 |
