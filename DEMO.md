# 60-second demo

The subject is the testing tool: seed, faults, the invariant that fired, and exact replay.

## Setup

Use a terminal at least 130 columns wide, because the violation lines are long. From the repo root, build first so compile output stays out of the recording.

```sh
cargo build --release
rm -rf repros
clear
```

## Storyboard

| Time | Command | What the viewer sees | Voiceover |
| --- | --- | --- | --- |
| 0:00 | 1 | 500 seeds: 213 pass, 287 fail, then a shrink from 3 faults to 1 | "Each seed is a synthetic logistics world with injected faults. This is a reasonable first-pass agent. Failures shrink to the faults that matter." |
| 0:14 | 2 | The run stops at seed 10 on `no-double-apply` | "Now stop at the first double receipt. One fault is enough." |
| 0:22 | 3 | Timeline with the timeout, the retry, and the red violation | "The receipt committed, but the agent saw a timeout and retried. 160 units became 320. The hash matches, so this failure is exactly reproducible." |
| 0:40 | 4 | v2 passes all 500 seeds | "Same seeds, same faults, careful policy." |
| 0:48 | 5 | Same repro under v2: it reads state and does not write again | "After the timeout it reads first and sees the receipt landed." |
| 0:54 | 6 | A recorded real model run replays with a matching hash | "The same harness can drive a real model, and replay its recorded answers offline." |

## Commands

```sh
# 1. Explore
cargo run -q --release -- explore --runs 500 --policy v1

# 2. Stop on a double update
cargo run -q --release -- explore --runs 500 --policy v1 --stop-on no-double-apply

# 3. Replay it (trace hash 3c3f8d0962092533)
cargo run -q --release -- replay repros/v1-seed-00010.json

# 4. Same seeds, careful policy
cargo run -q --release -- explore --runs 500 --policy v2

# 5. Same failure, careful policy
cargo run -q --release -- replay repros/v1-seed-00010.json --policy v2

# 6. Recorded LLM run, no model call
cargo run -q --release -- replay examples/llm-seed-00010.json
```

Commands 1, 2 and 3 exit with status 1 because they report violations. That is expected, and `cargo run -q` prints no extra error text for it.

## Recording tips

- Pause on the red `VIOLATION` lines in step 3. That is the key frame.
- Scripted output is deterministic, so a retake shows the same seeds, faults and hashes. Step 6 is deterministic too, because it replays a saved transcript.
